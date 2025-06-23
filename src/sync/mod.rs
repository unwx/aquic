pub(crate) mod rendezvous;

use std::any::type_name;
use tokio::sync::watch;

pub(crate) fn send_once<T>(sender: &watch::Sender<Option<T>>, value: T) -> bool {
    sender.send_if_modified(|current| {
        let modified = current.is_none();
        current.get_or_insert(value);
        modified
    })
}

pub(crate) async fn recv_init<T: Clone>(receiver: &mut watch::Receiver<Option<T>>) -> Option<T> {
    receiver.changed().await.ok()?;
    receiver.mark_unchanged();

    Some(receiver.borrow().clone().unwrap_or_else(|| {
        panic!(
            "{} is not initialized after a 'changed()' call",
            type_name::<watch::Receiver<Option<T>>>()
        )
    }))
}
