use {
    crate::wayland::{
        protocols::wayland::{wl_buffer::WlBuffer, wl_shm::WlShmFormat},
        tray::TraySurfaceId,
        Singletons, WlBufferHandler,
    },
    memfile::{MemFile, Seal},
    std::io::{self, Write},
    wl_client::proxy,
};

pub fn create_shm_buf_oneshot(
    s: &Singletons,
    data: &[u8],
    size: (i32, i32),
) -> Result<WlBuffer, io::Error> {
    create_shm_buf(s, data, size, None).map(|v| v.0)
}

pub fn create_shm_buf(
    s: &Singletons,
    data: &[u8],
    size: (i32, i32),
    surface: Option<TraySurfaceId>,
) -> Result<(WlBuffer, MemFile), io::Error> {
    let mut memfd = MemFile::create_sealable("wl-shm")?;
    memfd.add_seal(Seal::Shrink)?;
    memfd.write_all(data)?;
    let pool = s.wl_shm.create_pool(memfd.as_fd(), data.len() as _);
    let buffer = pool.create_buffer(0, size.0, size.1, size.0 * 4, WlShmFormat::ARGB8888);
    if let Some(id) = surface {
        proxy::set_event_handler(&buffer, WlBufferHandler { id });
    }
    pool.destroy();
    Ok((buffer, memfd))
}
//
// pub struct IconTick {
//     send: UnboundedSender<TrayItemId>,
// }
//
// impl IconTick {
//     pub fn new(sink: &EventSink) -> Self {
//         let sink = sink.clone();
//         let (send, mut recv) = unbounded_channel();
//         tokio::spawn(async move {
//             let mut interval = interval(Duration::from_secs(1));
//             interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
//             loop {
//                 let Some(first) = recv.recv().await else {
//                     return;
//                 };
//                 interval.tick().await;
//                 let mut elements = AHashSet::new();
//                 elements.insert(first);
//                 while let Ok(e) = recv.try_recv() {
//                     elements.insert(e);
//                 }
//                 sink.send(move |state| state.handle_tick(elements));
//             }
//         });
//         Self { send }
//     }
//
//     pub fn request_tick(&self, id: TrayItemId) {
//         let _ = self.send.send(id);
//     }
// }
