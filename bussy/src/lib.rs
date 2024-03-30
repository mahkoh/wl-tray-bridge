//! # bussy
//!
//! bussy is a simple interface layered on top of the low-level zbus interfaces.
//!
//! It provides the the following advantages:
//!
//! - bussy is completely async but does not require you to use async/await. This allows
//!   you to use bussy in environments where using async code directly is not possible.
//! - All outgoing messages are sent in the order in which you call the respective
//!   functions. This applies even if the function returns a future and you don't await
//!   it immediately.
//! - bussy supports pipelining. All requests are sent immediately even if you do not
//!   await the response. This reduces the number of roundtrips for n method calls from
//!   n to 1.
//! - When you handle a method call, you get a [PendingReply] object that you can hold on
//!   to for as long as you want without blocking any other progress. You can then reply
//!   to the call whenever you are ready.
//! - All incoming messages are handled in a single thread in order. This means that, if
//!   you get a reply to a method call and receive a signal, the order in which your code
//!   is invoked is exactly the same as the order in which the peer sent these message.
//!   (Note that, if you are using async/await syntax for method calls, then tokio's
//!   scheduling of tasks might get in the way of this.)
//!
//! Note the following caveats:
//!
//! - Introspection is not supported.
//!
//! # Example
//!
//! ```rust,no_run
//! # use zbus::names::*;
//! # use zbus::zvariant::*;
//! # async fn f() {
//! let zbus_conn = zbus::Connection::session().await.unwrap();
//! let conn_holder = bussy::Connection::wrap(&zbus_conn);
//! let conn = &conn_holder.connection;
//!
//! let res = conn.call::<String>(
//!     WellKnownName::from_static_str_unchecked("org.freedesktop.DBus"),
//!     InterfaceName::from_static_str_unchecked("org.freedesktop.DBus"),
//!     ObjectPath::from_static_str_unchecked("/org/freedesktop/DBus"),
//!     MemberName::from_static_str_unchecked("GetNameOwner"),
//!     &("org.freedesktop.DBus"), // the request body
//! ).await;
//! println!("The name org.freedesktop.DBus is owned by {}", res.unwrap());
//! # }
//! ```

use {
    error_reporter::Report,
    parking_lot::Mutex,
    pin_project::pin_project,
    serde::Serialize,
    std::{
        collections::HashMap,
        error::Error as StdError,
        future::Future,
        mem,
        num::NonZeroU32,
        pin::Pin,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
            Arc, Weak,
        },
        task::{Context, Poll},
    },
    thiserror::Error,
    tokio::{
        sync::{
            mpsc::{self, UnboundedReceiver},
            oneshot,
        },
        task::JoinHandle,
    },
    zbus::{
        export::futures_util::StreamExt,
        message::{Flags, Type},
        names::{BusName, InterfaceName, MemberName, UniqueName, WellKnownName},
        zvariant::{DynamicDeserialize, DynamicType, ObjectPath, OwnedValue, Str, Value},
        MatchRule, Message, MessageStream,
    },
};

/// A holder object for a bussy [Connection].
///
/// When this object is dropped, most resources are released and you can no longer use
/// the bussy [Connection].
#[non_exhaustive]
pub struct ConnectionHolder {
    /// The bussy [Connection].
    pub connection: Arc<Connection>,
}

impl Drop for ConnectionHolder {
    fn drop(&mut self) {
        self.connection.shared.kill();
    }
}

/// A bussy connection.
///
/// This object allows you to use the bussy interfaces. It is created from a
/// [zbus::Connection].
///
/// You can use both the bussy connection and the zbus connection at the same time.
pub struct Connection {
    shared: Arc<Shared>,
    kill_queue: mpsc::UnboundedSender<Message>,
}

type ObjectMethodKey = (InterfaceName<'static>, MemberName<'static>);
type ObjectMethodHandler = Arc<dyn Fn(PendingReply) + Send + Sync>;
type ObjectPropertyKey = (InterfaceName<'static>, MemberName<'static>);

struct ObjectData {
    path: ObjectPath<'static>,
    methods: Mutex<HashMap<ObjectMethodKey, ObjectMethodHandler>>,
    properties: Mutex<HashMap<ObjectPropertyKey, Value<'static>>>,
}

struct SignalHandlerData<T: ?Sized> {
    disabled: AtomicBool,
    match_rule: MatchRule<'static>,
    callback: T,
}

type MethodReplyHandler = Box<dyn FnOnce(Result<Message, Error>) + Send>;
type DynSignalHandler = Arc<SignalHandlerData<dyn Fn(&Message) + Send + Sync>>;

struct SharedMut {
    pending_replies: HashMap<NonZeroU32, MethodReplyHandler>,
    objects: HashMap<ObjectPath<'static>, Arc<ObjectData>>,
    weak_objects: HashMap<ObjectPath<'static>, Weak<Object>>,
    signal_handlers: HashMap<usize, DynSignalHandler>,
    send: Option<JoinHandle<()>>,
    recv: Option<JoinHandle<()>>,
}

struct Shared {
    shared: Mutex<SharedMut>,
    killed: AtomicBool,
    queue: mpsc::UnboundedSender<Message>,
}

/// An error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The bussy connection has already been destroyed.
    #[error("The dbus connection has been killed")]
    Killed,
    /// Could not send a message.
    #[error("Could not send a message")]
    Send(#[source] zbus::Error),
    /// An error message has no error name.
    #[error("Error message has no error name")]
    NoErrorName,
    /// An error message has no error body.
    #[error("Error message has no error body")]
    NoErrorBody(#[source] zbus::Error),
    /// A method call returned an error.
    #[error("The method call returned an error: {}: {}", .0, .1)]
    ErrorReply(String, String),
    /// Could not deserialize a message.
    #[error("Could not deserialize a message")]
    Deserialize(#[source] zbus::Error),
    /// Could not map a property value to the desired type.
    #[error("Could not map a property value to the desired type")]
    MapProperty(#[source] Box<dyn StdError + Sync + Send>),
}

const DBUS_PROPS_NAME: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.freedesktop.DBus.Properties");
const DBUS_NAME: WellKnownName<'static> =
    WellKnownName::from_static_str_unchecked("org.freedesktop.DBus");
const DBUS_INTERFACE: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.freedesktop.DBus");
const DBUS_PATH: ObjectPath<'static> =
    ObjectPath::from_static_str_unchecked("/org/freedesktop/DBus");
const NAME_OWNER_CHANGED: MemberName<'static> =
    MemberName::from_static_str_unchecked("NameOwnerChanged");
const REQUEST_NAME: MemberName<'static> = MemberName::from_static_str_unchecked("RequestName");
const GET: MemberName<'static> = MemberName::from_static_str_unchecked("Get");
const GET_ALL: MemberName<'static> = MemberName::from_static_str_unchecked("GetAll");
const ADD_MATCH: MemberName<'static> = MemberName::from_static_str_unchecked("AddMatch");
const REMOVE_MATCH: MemberName<'static> = MemberName::from_static_str_unchecked("RemoveMatch");

impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.kill();
    }
}

impl Shared {
    fn kill(&self) {
        let pending = {
            let mut shared = self.shared.lock();
            if let Some(task) = shared.recv.take() {
                task.abort();
            }
            if let Some(task) = shared.send.take() {
                task.abort();
            }
            self.killed.store(true, Relaxed);
            shared.signal_handlers.clear();
            shared.objects.clear();
            mem::take(&mut shared.pending_replies)
        };
        for (_, pending) in pending {
            pending(Err(Error::Killed));
        }
    }

    async fn send(
        self: Arc<Self>,
        connection: zbus::Connection,
        mut queue: UnboundedReceiver<Message>,
    ) {
        while let Some(msg) = queue.recv().await {
            if let Err(e) = connection.send(&msg).await {
                self.kill_reply(&msg, Error::Send(e));
                break;
            }
        }
        self.kill();
    }

    async fn recv(self: Arc<Self>, connection: zbus::Connection) {
        let mut stream = MessageStream::from(&connection);
        let mut signal_handlers = vec![];
        while let Some(msg) = stream.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    log::error!("Could not receive message: {}", Report::new(e));
                    break;
                }
            };
            let header = msg.header();
            match msg.message_type() {
                Type::MethodCall => {
                    let mut pr = PendingReply {
                        msg: msg.clone(),
                        shared: self.clone(),
                        replied: false,
                    };
                    let Some(interface) = header.interface() else {
                        continue;
                    };
                    let Some(path) = header.path() else {
                        continue;
                    };
                    let Some(member) = header.member() else {
                        continue;
                    };
                    let handler;
                    let get;
                    let get_all;
                    let object = {
                        let shared = self.shared.lock();
                        if self.killed.load(Relaxed) {
                            return;
                        }
                        shared.objects.get(path).cloned()
                    };
                    let Some(object) = object else {
                        pr.send_err("Object does not exist");
                        continue;
                    };
                    let handler = if interface == &DBUS_PROPS_NAME && member == &GET {
                        get = |pr: PendingReply| self.handle_get_property(&object, pr);
                        &get
                    } else if interface == &DBUS_PROPS_NAME && member == &GET_ALL {
                        get_all = |pr: PendingReply| self.handle_get_properties(&object, pr);
                        &get_all
                    } else {
                        handler = {
                            let methods = object.methods.lock();
                            methods.get(&(interface.clone(), member.clone())).cloned()
                        };
                        match handler.as_ref() {
                            Some(h) => &**h,
                            _ => {
                                pr.send_err("Method does not exist");
                                continue;
                            }
                        }
                    };
                    handler(pr);
                }
                Type::MethodReturn | Type::Error => {
                    let Some(serial) = msg.header().reply_serial() else {
                        continue;
                    };
                    let pending = {
                        let mut shared = self.shared.lock();
                        if self.killed.load(Relaxed) {
                            return;
                        }
                        shared.pending_replies.remove(&serial)
                    };
                    let Some(pending) = pending else {
                        continue;
                    };
                    if msg.message_type() == Type::Error {
                        'handle_error: {
                            let Some(name) = header.error_name() else {
                                pending(Err(Error::NoErrorName));
                                break 'handle_error;
                            };
                            let s = match msg.body().deserialize::<String>() {
                                Ok(s) => s,
                                Err(e) => {
                                    pending(Err(Error::NoErrorBody(e)));
                                    break 'handle_error;
                                }
                            };
                            pending(Err(Error::ErrorReply(name.to_string(), s)));
                        }
                    } else {
                        pending(Ok(msg));
                    }
                }
                Type::Signal => {
                    {
                        let shared = self.shared.lock();
                        if self.killed.load(Relaxed) {
                            return;
                        }
                        for handler in shared.signal_handlers.values() {
                            if handler.match_rule.matches(&msg) == Ok(true) {
                                signal_handlers.push(handler.clone());
                            }
                        }
                    };
                    while let Some(handler) = signal_handlers.pop() {
                        if self.killed.load(Relaxed) {
                            return;
                        }
                        if handler.disabled.load(Relaxed) {
                            continue;
                        }
                        (handler.callback)(&msg);
                    }
                }
            }
        }
        self.kill();
    }

    fn handle_get_property(self: &Arc<Self>, object: &Arc<ObjectData>, pr: PendingReply) {
        handle_call(
            pr,
            |(interface, property): (String, String), mut pr: PendingReply| {
                let Ok(interface) = InterfaceName::try_from(&*interface) else {
                    pr.send_err("Invalid interface name");
                    return;
                };
                let Ok(member) = MemberName::try_from(&*property) else {
                    pr.send_err("Invalid member name");
                    return;
                };
                let prop = object
                    .properties
                    .lock()
                    .get(&(interface, member))
                    .map(|v| v.try_clone().unwrap());
                match prop {
                    None => pr.send_err("Property does not exist"),
                    Some(p) => {
                        pr.send(&p);
                    }
                }
            },
        );
    }

    fn handle_get_properties(self: &Arc<Self>, object: &Arc<ObjectData>, pr: PendingReply) {
        handle_call(pr, |interface: String, mut pr: PendingReply| {
            let Ok(interface) = InterfaceName::try_from(&*interface) else {
                pr.send_err("Invalid interface name");
                return;
            };
            let properties = object.properties.lock();
            let mut dict = HashMap::new();
            for ((intf, member), prop) in &*properties {
                if &interface == intf {
                    dict.insert(member.as_str(), prop);
                }
            }
            pr.send(&dict);
        });
    }

    fn kill_reply(&self, msg: &Message, e: Error) {
        let Some(pending) = self
            .shared
            .lock()
            .pending_replies
            .remove(&msg.primary_header().serial_num())
        else {
            return;
        };
        pending(Err(e));
    }

    async fn kill_queue(self: Arc<Self>, mut queue: UnboundedReceiver<Message>) {
        while let Some(msg) = queue.recv().await {
            self.kill_reply(&msg, Error::Killed);
        }
    }

    fn send_signal(
        &self,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        method: MemberName<'_>,
        body: &(impl Serialize + DynamicType),
    ) {
        let message = Message::signal(
            path.into_owned(),
            interface.into_owned(),
            method.into_owned(),
        )
        .unwrap()
        .build(body)
        .unwrap();
        let _ = self.queue.send(message);
    }

    fn call_no_reply(
        &self,
        destination: BusName<'_>,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        method: MemberName<'_>,
        body: &(impl Serialize + DynamicType),
    ) {
        let message = Message::method(path.into_owned(), method.into_owned())
            .unwrap()
            .destination(destination.into_owned())
            .unwrap()
            .interface(interface.into_owned())
            .unwrap()
            .with_flags(Flags::NoReplyExpected)
            .unwrap()
            .build(body)
            .unwrap();
        let _ = self.queue.send(message);
    }

    #[allow(clippy::too_many_arguments)]
    fn call_async<CB, R>(
        self: &Arc<Self>,
        destination: BusName<'_>,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        method: MemberName<'_>,
        body: &(impl Serialize + DynamicType),
        kill_queue: &mpsc::UnboundedSender<Message>,
        callback: CB,
    ) -> Call
    where
        CB: FnOnce(Result<R, Error>) + Send + 'static,
        R: for<'a> DynamicDeserialize<'a> + 'static,
    {
        let message = Message::method(path.into_owned(), method.into_owned())
            .unwrap()
            .destination(destination.into_owned())
            .unwrap()
            .interface(interface.into_owned())
            .unwrap()
            .build(body)
            .unwrap();
        let serial = message.primary_header().serial_num();
        let callback = Box::new(move |res: Result<Message, Error>| {
            let msg = res.and_then(|msg| {
                let body = msg.body();
                let v = body.deserialize();
                v.map_err(Error::Deserialize)
            });
            callback(msg);
        });
        {
            let mut shared = self.shared.lock();
            shared.pending_replies.insert(serial, callback);
        }
        let _ = if self.killed.load(Relaxed) {
            kill_queue.send(message)
        } else {
            self.queue.send(message)
        };
        Call {
            serial,
            shared: self.clone(),
            detached: false,
        }
    }

    fn call<R>(
        self: &Arc<Self>,
        destination: BusName<'_>,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        method: MemberName<'_>,
        kill_queue: &mpsc::UnboundedSender<Message>,
        body: &(impl Serialize + DynamicType),
    ) -> CallFuture<R>
    where
        R: for<'a> DynamicDeserialize<'a> + Send + 'static,
    {
        let (send, recv) = oneshot::channel();
        let call = self.call_async(
            destination,
            interface,
            path,
            method,
            body,
            kill_queue,
            |res| {
                let _ = send.send(res);
            },
        );
        CallFuture { call, recv }
    }

    fn handle_signal<CB, B>(
        self: &Arc<Self>,
        match_rule: MatchRule<'static>,
        kill_queue: &mpsc::UnboundedSender<Message>,
        callback: CB,
    ) -> SignalHandler
    where
        B: for<'a> DynamicDeserialize<'a> + Send + 'static,
        CB: Fn(B) + Send + Sync + 'static,
    {
        let callback = move |msg: &Message| {
            let body = msg.body().deserialize().map_err(Error::Deserialize);
            match body {
                Ok(body) => callback(body),
                Err(e) => {
                    log::error!("Could not deserialize signal body: {}", Report::new(e));
                }
            }
        };
        let rule = match_rule.to_string();
        static ID: AtomicUsize = AtomicUsize::new(0);
        let id = ID.fetch_add(1, Relaxed);
        {
            let data = Arc::new(SignalHandlerData {
                disabled: Default::default(),
                match_rule,
                callback,
            });
            let mut shared = self.shared.lock();
            if !self.killed.load(Relaxed) {
                shared.signal_handlers.insert(id, data);
            }
        }
        self.call_async::<_, ()>(
            DBUS_NAME.clone().into(),
            DBUS_INTERFACE,
            DBUS_PATH,
            ADD_MATCH,
            &rule,
            kill_queue,
            move |res| {
                if let Err(e) = res {
                    log::error!("Could not register a signal handler: {}", Report::new(e),);
                }
            },
        )
        .detach();
        SignalHandler {
            id,
            shared: self.clone(),
            rule,
            detached: false,
        }
    }

    fn add_obj(self: &Arc<Self>, path: ObjectPath<'_>) -> Arc<Object> {
        let mut shared = self.shared.lock();
        if let Some(obj) = shared.weak_objects.get(&path) {
            if let Some(obj) = obj.upgrade() {
                return obj;
            }
        }
        let obj = Arc::new(Object {
            shared: self.clone(),
            data: Arc::new(ObjectData {
                path: path.to_owned(),
                methods: Default::default(),
                properties: Default::default(),
            }),
        });
        if !self.killed.load(Relaxed) {
            shared.objects.insert(path.to_owned(), obj.data.clone());
            shared
                .weak_objects
                .insert(path.into_owned(), Arc::downgrade(&obj));
        }
        obj
    }

    fn request_name(&self, name: WellKnownName<'_>) {
        self.call_no_reply(
            DBUS_NAME.into(),
            DBUS_INTERFACE,
            DBUS_PATH,
            REQUEST_NAME,
            &(name.as_str(), 0u32),
        );
    }

    fn get_property_async<CB, R>(
        self: &Arc<Self>,
        destination: BusName<'_>,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        member: MemberName<'_>,
        kill_queue: &mpsc::UnboundedSender<Message>,
        callback: CB,
    ) -> Call
    where
        CB: FnOnce(Result<R, Error>) + Send + 'static,
        R: TryFrom<OwnedValue>,
        R::Error: StdError + Send + Sync + 'static,
    {
        self.call_async(
            destination,
            DBUS_PROPS_NAME,
            path,
            GET,
            &(interface.as_str(), member.as_str()),
            kill_queue,
            move |v: Result<OwnedValue, _>| {
                callback(
                    v.and_then(|v| R::try_from(v).map_err(|e| Error::MapProperty(Box::new(e)))),
                );
            },
        )
    }

    fn get_property<R>(
        self: &Arc<Self>,
        destination: BusName<'_>,
        interface: InterfaceName<'_>,
        path: ObjectPath<'_>,
        member: MemberName<'_>,
        kill_queue: &mpsc::UnboundedSender<Message>,
    ) -> CallFuture<R>
    where
        R: TryFrom<OwnedValue> + Send + 'static,
        R::Error: StdError + Send + Sync + 'static,
    {
        let (send, recv) = oneshot::channel();
        let call =
            self.get_property_async(destination, interface, path, member, kill_queue, |res| {
                let _ = send.send(res);
            });
        CallFuture { call, recv }
    }
}

impl Connection {
    /// Wraps a [zbus::Connection] in a bussy [Connection].
    ///
    /// Note that the bussy connection can only be used while you are holding on to the
    /// [ConnectionHolder].
    ///
    /// You can use both the zbus connection and the bussy connection at the same time.
    pub fn wrap(connection: &zbus::Connection) -> ConnectionHolder {
        let (send, recv) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            shared: Mutex::new(SharedMut {
                pending_replies: Default::default(),
                objects: Default::default(),
                weak_objects: Default::default(),
                signal_handlers: Default::default(),
                send: None,
                recv: None,
            }),
            killed: Default::default(),
            queue: send,
        });
        {
            let mut shared_mut = shared.shared.lock();
            let send = tokio::spawn(shared.clone().send(connection.clone(), recv));
            let recv = tokio::spawn(shared.clone().recv(connection.clone()));
            shared_mut.send = Some(send);
            shared_mut.recv = Some(recv);
        }
        let (send, recv) = mpsc::unbounded_channel();
        tokio::spawn(shared.clone().kill_queue(recv));
        let dbus = Self {
            shared,
            kill_queue: send,
        };
        ConnectionHolder {
            connection: Arc::new(dbus),
        }
    }

    /// Sends a signal.
    pub fn send_signal<'a>(
        &self,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        method: impl Into<MemberName<'a>>,
        body: &(impl Serialize + DynamicType),
    ) {
        self.shared
            .send_signal(interface.into(), path.into(), method.into(), body)
    }

    /// Calls a method without expecting a reply.
    pub fn call_no_reply<'a>(
        &self,
        destination: impl Into<BusName<'a>>,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        method: impl Into<MemberName<'a>>,
        body: &(impl Serialize + DynamicType),
    ) {
        self.shared.call_no_reply(
            destination.into(),
            interface.into(),
            path.into(),
            method.into(),
            body,
        )
    }

    /// Calls a method and waits for the reply with a callback.
    ///
    /// The returned [Call] object represents this call. If it is dropped, the callback
    /// will not be called even if a reply arrives. But see [Call::detach].
    pub fn call_async<'a, R>(
        &self,
        destination: impl Into<BusName<'a>>,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        method: impl Into<MemberName<'a>>,
        body: &(impl Serialize + DynamicType),
        callback: impl FnOnce(Result<R, Error>) + Send + 'static,
    ) -> Call
    where
        R: for<'b> DynamicDeserialize<'b> + 'static,
    {
        self.shared.call_async(
            destination.into(),
            interface.into(),
            path.into(),
            method.into(),
            body,
            &self.kill_queue,
            callback,
        )
    }

    /// Calls a method and returns a future that can be used to await the reply.
    ///
    /// Note that this function is not async. The method is called immediately when you
    /// call this function. Awaiting the returned future is only necessary for receiving
    /// the reply.
    pub fn call<'a, R>(
        &self,
        destination: impl Into<BusName<'a>>,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        method: impl Into<MemberName<'a>>,
        body: &(impl Serialize + DynamicType),
    ) -> CallFuture<R>
    where
        R: for<'b> DynamicDeserialize<'b> + Send + 'static,
    {
        self.shared.call(
            destination.into(),
            interface.into(),
            path.into(),
            method.into(),
            &self.kill_queue,
            body,
        )
    }

    /// Installs a signal handler.
    ///
    /// The returned [SignalHandler] represents the installed handler. If it is dropped,
    /// the signal handler is uninstalled. But see [SignalHandler::detach].
    pub fn handle_signal<'a, B>(
        &self,
        interface: impl Into<InterfaceName<'a>>,
        signal: impl Into<MemberName<'a>>,
        callback: impl Fn(B) + Send + Sync + 'static,
    ) -> SignalHandler
    where
        B: for<'b> DynamicDeserialize<'b> + Send + 'static,
    {
        let match_rule = MatchRuleBuilder::default()
            .msg_type(Type::Signal)
            .interface(interface.into().into_owned())
            .member(signal.into().into_owned())
            .build();
        self.shared
            .handle_signal(match_rule, &self.kill_queue, callback)
    }

    /// Intercepts dbus messages.
    ///
    /// This is a more general from of [Self::handle_signal] and allows you to intercept
    /// arbitrary messages.
    ///
    /// The same rules for the returned [SignalHandler] apply.
    ///
    /// You can construct the [MatchRule] easily by using a [MatchRuleBuilder].
    pub fn handle_messages<B>(
        &self,
        match_rule: MatchRule<'_>,
        callback: impl Fn(B) + Send + Sync + 'static,
    ) -> SignalHandler
    where
        B: for<'b> DynamicDeserialize<'b> + Send + 'static,
    {
        self.shared
            .handle_signal(match_rule.into_owned(), &self.kill_queue, callback)
    }

    /// Exports an object at a path.
    ///
    /// The returned object represents the exported object. Calling this method multiple
    /// times with the same path will return the same object until all instance of it have
    /// been dropped. At that point the object is unexported.
    pub fn add_obj<'a>(&self, path: impl Into<ObjectPath<'a>>) -> Arc<Object> {
        self.shared.add_obj(path.into())
    }

    /// Adds a handler for `NameOwnerChanged` signal.
    ///
    /// This is a convenience method around [Self::handle_signal]. See that method for
    /// more details.
    pub fn on_name_owner_changed(
        &self,
        f: impl Fn(String, String, String) + Send + Sync + 'static,
    ) -> SignalHandler {
        self.handle_signal(
            DBUS_INTERFACE,
            NAME_OWNER_CHANGED,
            move |(name, old_owner, new_owner): (String, String, String)| {
                f(name, old_owner, new_owner)
            },
        )
    }

    /// Requests a name.
    pub fn request_name<'a>(&self, name: impl Into<WellKnownName<'a>>) {
        self.shared.request_name(name.into())
    }

    /// Retrieves a property and waits for the reply with a callback.
    ///
    /// This is a convenience method around [Self::call_async]. See that method for more
    /// details.
    pub fn get_property_async<'a, R>(
        &self,
        destination: impl Into<BusName<'a>>,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        member: impl Into<MemberName<'a>>,
        callback: impl FnOnce(Result<R, Error>) + Send + 'static,
    ) -> Call
    where
        R: TryFrom<OwnedValue>,
        R::Error: StdError + Send + Sync + 'static,
    {
        self.shared.get_property_async(
            destination.into(),
            interface.into(),
            path.into(),
            member.into(),
            &self.kill_queue,
            callback,
        )
    }

    /// Retrieves a property and returns a future that can be used to await the value.
    ///
    /// This is a convenience method around [Self::call]. See that method for more
    /// details.
    pub fn get_property<'a, R>(
        &self,
        destination: impl Into<BusName<'a>>,
        interface: impl Into<InterfaceName<'a>>,
        path: impl Into<ObjectPath<'a>>,
        member: impl Into<MemberName<'a>>,
    ) -> CallFuture<R>
    where
        R: TryFrom<OwnedValue> + Send + 'static,
        R::Error: StdError + Send + Sync + 'static,
    {
        self.shared.get_property(
            destination.into(),
            interface.into(),
            path.into(),
            member.into(),
            &self.kill_queue,
        )
    }
}

/// An installed signal handler.
///
/// Dropping this object causes the signal handler to be uninstalled unless you call
/// [Self::detach].
#[must_use]
pub struct SignalHandler {
    id: usize,
    shared: Arc<Shared>,
    rule: String,
    detached: bool,
}

impl SignalHandler {
    /// Detaches the signal handler from this object.
    ///
    /// The signal handler will not be uninstalled when this object is dropped.
    pub fn detach(&mut self) {
        self.detached = true;
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        if !self.detached {
            self.shared.call_no_reply(
                DBUS_NAME.clone().into(),
                DBUS_INTERFACE,
                DBUS_PATH,
                REMOVE_MATCH,
                &self.rule,
            );
            if let Some(data) = self.shared.shared.lock().signal_handlers.remove(&self.id) {
                data.disabled.store(true, Relaxed);
            }
        }
    }
}

/// An exported object.
///
/// Dropping this object causes the object to be unexported.
#[must_use]
pub struct Object {
    shared: Arc<Shared>,
    data: Arc<ObjectData>,
}

impl Drop for Object {
    fn drop(&mut self) {
        self.shared.shared.lock().objects.remove(&self.data.path);
    }
}

impl Object {
    /// Sets a property.
    ///
    /// This function handles emitting `PropertiesChanged` signals.
    pub fn set_property<'a, B>(
        &self,
        interface: impl Into<InterfaceName<'a>>,
        member: impl Into<MemberName<'a>>,
        value: B,
    ) where
        B: Into<Value<'static>>,
    {
        let interface = interface.into();
        let member = member.into();
        let value = value.into();
        self.data.properties.lock().insert(
            (interface.to_owned(), member.to_owned()),
            value.try_clone().unwrap(),
        );
        let mut changed = HashMap::new();
        changed.insert(member.to_string(), value);
        let invalidated: Vec<String> = vec![];
        static CHANGED: MemberName<'static> =
            MemberName::from_static_str_unchecked("PropertiesChanged");
        let msg = Message::signal(
            self.data.path.clone(),
            DBUS_PROPS_NAME.clone(),
            CHANGED.clone(),
        )
        .unwrap()
        .build(&(interface.to_string(), changed, invalidated))
        .unwrap();
        let _ = self.shared.queue.send(msg);
    }

    /// Adds a method handler.
    ///
    /// The [PendingReply] passed into the callback should be used to reply to the call
    /// with the expected value or an error.
    pub fn add_method<'a, B>(
        &self,
        interface: impl Into<InterfaceName<'a>>,
        method: impl Into<MemberName<'a>>,
        callback: impl Fn(B, PendingReply) + Send + Sync + 'static,
    ) where
        B: for<'b> DynamicDeserialize<'b> + Send + 'static,
    {
        let interface = interface.into();
        let method = method.into();
        let handle = Arc::new(move |pr: PendingReply| handle_call(pr, &callback));
        self.data
            .methods
            .lock()
            .insert((interface.to_owned(), method.to_owned()), handle);
    }
}

fn handle_call<CB, B>(mut pr: PendingReply, callback: CB)
where
    B: for<'a> DynamicDeserialize<'a> + Send + 'static,
    CB: Fn(B, PendingReply),
{
    let body = pr.msg.body().deserialize().map_err(Error::Deserialize);
    match body {
        Ok(body) => callback(body, pr),
        Err(e) => {
            log::error!("Could not deserialize a message: {}", Report::new(e));
            pr.send_err("Could not deserialize message body");
        }
    }
}

/// A pending call.
///
/// Dropping this object will cause the callback to not be called unless you call
/// [Self::detach].
#[must_use]
pub struct Call {
    serial: NonZeroU32,
    shared: Arc<Shared>,
    detached: bool,
}

impl Call {
    /// Detaches this object from the pending call.
    ///
    /// After calling this, the callback will be called even if you drop this object.
    pub fn detach(&mut self) {
        self.detached = true;
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.detached {
            self.shared
                .shared
                .lock()
                .pending_replies
                .remove(&self.serial);
        }
    }
}

/// A future representing a method call response.
#[pin_project]
pub struct CallFuture<T> {
    call: Call,
    #[pin]
    recv: oneshot::Receiver<Result<T, Error>>,
}

impl<T> Future for CallFuture<T> {
    type Output = Result<T, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project()
            .recv
            .poll(cx)
            .map(|v| v.unwrap_or(Err(Error::Killed)))
    }
}

/// A pending reply to a method call.
///
/// Use this object to reply to method calls. You can hang on to this object for as long
/// as you like.
pub struct PendingReply {
    msg: Message,
    shared: Arc<Shared>,
    replied: bool,
}

impl PendingReply {
    /// Returns the request message.
    pub fn message(&self) -> &Message {
        &self.msg
    }

    /// Returns the name of the sender.
    pub fn sender(&self) -> Option<BusName<'static>> {
        self.msg.header().sender().map(|u| u.to_owned().into())
    }

    /// Sends a success reply.
    pub fn send(&mut self, body: &(impl Serialize + DynamicType)) {
        let msg = Message::method_reply(&self.msg)
            .unwrap()
            .build(body)
            .unwrap();
        let _ = self.shared.queue.send(msg);
        self.replied = true;
    }

    /// Sends an error reply.
    pub fn send_err(&mut self, msg: &str) {
        let msg = Message::method_error(&self.msg, "Bussy.Unspecified")
            .unwrap()
            .build(&msg)
            .unwrap();
        let _ = self.shared.queue.send(msg);
        self.replied = true;
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        let no_reply = self
            .msg
            .primary_header()
            .flags()
            .contains(Flags::NoReplyExpected);
        if !self.replied && !no_reply {
            self.send_err("Application did not send a reply");
        }
    }
}

/// A builder for a [MatchRule].
///
/// This is a simple wrapper that allows you to not have to call [Result::unwrap] if a
/// build step cannot fail.
pub struct MatchRuleBuilder<'m>(zbus::MatchRuleBuilder<'m>);

impl Default for MatchRuleBuilder<'_> {
    fn default() -> Self {
        Self(MatchRule::builder())
    }
}

impl<'m> MatchRuleBuilder<'m> {
    pub fn build(self) -> MatchRule<'m> {
        self.0.build()
    }

    pub fn sender<B>(self, sender: B) -> Self
    where
        B: Into<BusName<'m>>,
    {
        Self(self.0.sender(sender).unwrap())
    }

    pub fn msg_type(self, msg_type: Type) -> Self {
        Self(self.0.msg_type(msg_type))
    }

    pub fn interface<I>(self, interface: I) -> Self
    where
        I: Into<InterfaceName<'m>>,
    {
        Self(self.0.interface(interface).unwrap())
    }

    pub fn member<M>(self, member: M) -> Self
    where
        M: Into<MemberName<'m>>,
    {
        Self(self.0.member(member).unwrap())
    }

    pub fn path<P>(self, path: P) -> Self
    where
        P: Into<ObjectPath<'m>>,
    {
        Self(self.0.path(path).unwrap())
    }

    pub fn path_namespace<P>(self, path_namespace: P) -> Self
    where
        P: Into<ObjectPath<'m>>,
    {
        Self(self.0.path_namespace(path_namespace).unwrap())
    }

    pub fn destination<B>(self, destination: B) -> Self
    where
        B: Into<UniqueName<'m>>,
    {
        Self(self.0.destination(destination).unwrap())
    }

    pub fn add_arg<S>(self, arg: S) -> zbus::Result<Self>
    where
        S: Into<Str<'m>>,
    {
        Ok(Self(self.0.add_arg(arg)?))
    }

    pub fn arg<S>(self, idx: u8, arg: S) -> zbus::Result<Self>
    where
        S: Into<Str<'m>>,
    {
        Ok(Self(self.0.arg(idx, arg)?))
    }

    pub fn add_arg_path<P>(self, arg_path: P) -> zbus::Result<Self>
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<zbus::Error>,
    {
        Ok(Self(self.0.add_arg_path(arg_path)?))
    }

    pub fn arg_path<P>(self, idx: u8, arg_path: P) -> zbus::Result<Self>
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<zbus::Error>,
    {
        Ok(Self(self.0.arg_path(idx, arg_path)?))
    }

    pub fn arg0ns<S>(self, namespace: S) -> zbus::Result<Self>
    where
        S: Into<Str<'m>>,
    {
        Ok(Self(self.0.arg0ns(namespace)?))
    }
}
