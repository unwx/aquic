use crate::conditional;


pub mod oneshot;
pub mod stream;


pub(crate) mod mpsc;
pub(crate) mod rpc;


conditional! {
    multithread,

    use std::sync::Arc;

    /// Reference-counting pointer, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type SmartRc<T> = Arc<T>;
}

conditional! {
    not(multithread),

    use std::rc::Rc;

    /// Reference-counting pointer, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type SmartRc<T> = Rc<T>;
}
