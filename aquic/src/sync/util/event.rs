use intrusive_collections::{LinkedList, LinkedListLink, UnsafeRef, intrusive_adapter};
use std::cell::RefCell;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

struct Waiter {
    link: LinkedListLink,
    state: RefCell<WaiterState>,
}

struct WaiterState {
    waker: Option<Waker>,
    notified: bool,
}

intrusive_adapter!(WaiterAdapter = UnsafeRef<Waiter>: Waiter { link => LinkedListLink });


struct EventState {
    list: LinkedList<WaiterAdapter>,
    epoch: usize,
}

impl Debug for EventState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventState")
            .field("list", &"...")
            .field("epoch", &self.epoch)
            .finish()
    }
}


/// `!Send` alternative to `event-listener::Event`.
///
/// It's a simplified implementation, that allows `notify_all()` only at the moment.
#[derive(Debug, Clone)]
pub(crate) struct Event {
    state: Rc<RefCell<EventState>>,
}

impl Event {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(EventState {
                list: LinkedList::new(WaiterAdapter::new()),
                epoch: 0,
            })),
        }
    }

    /// Create an event listener.
    ///
    /// `notify()` method may notify this listener
    /// even if it wasn't suspend in `.await` yet.
    pub fn listen(&self) -> EventListener {
        EventListener {
            event_state: self.state.clone(),
            target_epoch: self.state.borrow().epoch,
            waiter: Waiter {
                link: LinkedListLink::new(),
                state: RefCell::new(WaiterState {
                    waker: None,
                    notified: false,
                }),
            },
        }
    }

    /// Notify all existing listeners.
    pub fn notify_all(&self) {
        let mut state = self.state.borrow_mut();
        state.epoch = state.epoch.wrapping_add(1);

        while let Some(waiter) = state.list.pop_front() {
            let mut w_state = waiter.state.borrow_mut();
            w_state.notified = true;

            if let Some(waker) = w_state.waker.take() {
                waker.wake();
            }
        }
    }
}


pub(crate) struct EventListener {
    event_state: Rc<RefCell<EventState>>,
    target_epoch: usize,
    waiter: Waiter,
}

impl Future for EventListener {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &*self;

        if this.event_state.borrow().epoch != this.target_epoch {
            return Poll::Ready(());
        }

        {
            let mut w_state = this.waiter.state.borrow_mut();

            if w_state.notified {
                return Poll::Ready(());
            }

            w_state.waker = Some(cx.waker().clone());
        }

        if !this.waiter.link.is_linked() {
            let mut state = this.event_state.borrow_mut();
            let ptr = &this.waiter as *const Waiter;

            // SAFETY: `this` is pinned by `Pin<&mut Self>`.
            // `waiter` will not move in memory until `EventListener` is dropped.
            state.list.push_back(unsafe { UnsafeRef::from_raw(ptr) });
        }

        Poll::Pending
    }
}

impl Drop for EventListener {
    fn drop(&mut self) {
        if self.waiter.link.is_linked() {
            let mut state = self.event_state.borrow_mut();
            let ptr = &self.waiter as *const Waiter;

            // SAFETY: We verified the pointer is inside the list via `is_linked()`.
            let mut cursor = unsafe { state.list.cursor_mut_from_ptr(ptr) };
            cursor.remove();
        }
    }
}
