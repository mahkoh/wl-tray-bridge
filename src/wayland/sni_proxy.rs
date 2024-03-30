use {
    crate::{
        sni::{self, MutableProperty, SniItem, SniItemOwner, SniMenuDelta},
        wayland::State,
    },
    std::{
        sync::Arc,
        task::{Context, Poll},
    },
    tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};

pub struct EventStream {
    recv: UnboundedReceiver<Action>,
}

#[derive(Clone)]
pub struct EventSink {
    send: UnboundedSender<Action>,
}

pub fn event_stream() -> (EventSink, EventStream) {
    let (send, recv) = unbounded_channel();
    let sink = EventSink { send };
    let stream = EventStream { recv };
    (sink, stream)
}

impl EventSink {
    pub fn send(&self, action: impl FnOnce(&mut State) + Send + 'static) {
        let _ = self.send.send(Box::new(action));
    }
}

impl EventStream {
    pub fn poll(&mut self, cx: &mut Context<'_>, state: &mut State) {
        while let Poll::Ready(Some(action)) = self.recv.poll_recv(cx) {
            action(state);
        }
    }
}

pub fn spawn(conn: &Arc<bussy::Connection>, sink: &EventSink) {
    let sink = sink.clone();
    sni::spawn(conn, move |item| handle_new_item(&sink, item))
}

type Action = Box<dyn FnOnce(&mut State) + Send>;

fn handle_new_item(sink: &EventSink, item: &Arc<SniItem>) {
    item.set_owner(Box::new(Owner {
        item: item.clone(),
        sink: sink.clone(),
    }));
    let item = item.clone();
    sink.send(move |state| {
        state.handle_new_sni_item(item);
    });
}

struct Owner {
    item: Arc<SniItem>,
    sink: EventSink,
}

impl SniItemOwner for Owner {
    fn removed(&self) {
        let item = self.item.clone();
        self.sink.send(move |state| {
            state.handle_sni_item_removed(&item);
        });
    }

    fn property_changed(&self, prop: MutableProperty) {
        let item = self.item.clone();
        self.sink.send(move |state| {
            state.handle_sni_item_prop_changed(&item, prop);
        });
    }

    fn menu_changed(&self, delta: SniMenuDelta) {
        let item = self.item.clone();
        self.sink.send(move |state| {
            state.handle_sni_menu_changed(&item, delta);
        });
    }
}
