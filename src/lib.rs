#![forbid(unsafe_code)]
use std::future::Future;

pub mod prelude {
    pub use super::{MiniGen, MiniIter, Yielder, generator, iterator};
}

mod yielder {
    use std::task::Poll;

    pub struct Yielder<Y> {
        pub(crate) sender: Sender<Y>,
    }

    impl<Y> Yielder<Y> {
        pub fn yield_value(&mut self, value: Y) -> YieldFuture<'_, Y> {
            YieldFuture {
                sender: &mut self.sender,
                value: Some(value),
            }
        }
    }

    #[cfg(not(feature = "send"))]
    #[repr(transparent)]
    pub(crate) struct Sender<T>(pub(crate) std::rc::Rc<std::cell::Cell<Option<T>>>);

    #[cfg(feature = "send")]
    pub(crate) struct Sender<T>(pub(crate) std::sync::Arc<std::sync::Mutex<Option<T>>>);

    #[cfg(not(feature = "send"))]
    impl<T> Sender<T> {
        pub(crate) fn send(&mut self, val: T) {
            let old = self.0.replace(Some(val));
            assert!(old.is_none(), "sending, but channel isn't empty!");
        }
    }

    #[cfg(feature = "send")]
    impl<T> Sender<T> {
        pub(crate) fn send(&mut self, val: T) {
            let mut lock = self.0.try_lock().expect("Calling thread should be waiting");
            let old = lock.replace(val);
            assert!(old.is_none(), "sending, but channel isn't empty!");
        }
    }

    #[cfg(not(feature = "send"))]
    #[repr(transparent)]
    pub(crate) struct Receiver<T>(pub(crate) std::rc::Rc<std::cell::Cell<Option<T>>>);

    #[cfg(feature = "send")]
    #[repr(transparent)]
    pub(crate) struct Receiver<T>(pub(crate) std::sync::Arc<std::sync::Mutex<Option<T>>>);

    #[cfg(not(feature = "send"))]
    impl<T> Receiver<T> {
        pub(crate) fn try_recv(&mut self) -> Option<T> {
            self.0.take()
        }
    }
    #[cfg(feature = "send")]
    impl<T> Receiver<T> {
        pub(crate) fn try_recv(&mut self) -> Option<T> {
            let mut lock = self.0.try_lock().expect("Calling thread should be waiting");
            lock.take()
        }
    }

    #[must_use = "YieldFuture must be awaited to yield a value!"]
    pub struct YieldFuture<'a, Y> {
        sender: &'a mut Sender<Y>,
        value: Option<Y>,
    }

    impl<Y> Unpin for YieldFuture<'_, Y> {}

    impl<Y> Future for YieldFuture<'_, Y> {
        type Output = ();

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match self.value.take() {
                // First time being polled, yield so the caller (of the generator) regains control
                Some(v) => {
                    self.sender.send(v);
                    Poll::Pending
                }
                // Second time being polled
                None => Poll::Ready(()),
            }
        }
    }
}

mod generator {
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Wake},
    };

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    pub use backend::MiniGen;
    mod backend {
        use std::{
            pin::Pin,
            sync::{Arc, LazyLock},
            task::{Context, Poll, Waker},
        };

        use crate::{Receiver, generator::NoopWake};

        use super::GeneratorStatus;

        pub struct MiniGen<Y, R, Fut: Future<Output = R>> {
            pub(crate) future: Pin<Box<Fut>>,
            pub(crate) receiver: Receiver<Y>,
            pub(crate) finished: bool,
        }
        impl<Y, R, Fut: Future<Output = R>> MiniGen<Y, R, Fut> {
            pub fn resume(&mut self) -> GeneratorStatus<Y, R> {
                // The future was polled to completion before, so we should act similar to a
                // fused iterator and return that it's completed
                if self.finished {
                    return GeneratorStatus::Completed;
                }
                // We don't actually need a proper "waker" so we can just sort of no-op it
                static WAKER: LazyLock<Waker> = LazyLock::new(|| Waker::from(Arc::new(NoopWake)));
                let mut context = Context::from_waker(&WAKER);
                loop {
                    let pinned = self.future.as_mut();
                    // We poll the future we're storing
                    match pinned.poll(&mut context) {
                        // Future has returned, generator has completed.
                        Poll::Ready(v) => {
                            self.finished = true;
                            return GeneratorStatus::Returned(v);
                        }
                        // Yield or a random await
                        Poll::Pending => {
                            // We check here, because the future might yield for some reason other than
                            // stream.yield_value()
                            if let Some(v) = self.receiver.try_recv() {
                                return GeneratorStatus::Yielded(v);
                            }
                        }
                    }
                }
            }

            pub fn resume_async(&mut self) -> MiniGenFuture<'_, Y, R, Fut> {
                MiniGenFuture {
                    generator: &mut *self,
                }
            }
        }

        #[must_use = "Must be awaited to produce a value!"]
        pub struct MiniGenFuture<'r, Y, R, Fut: Future<Output = R>> {
            generator: &'r mut MiniGen<Y, R, Fut>,
        }

        impl<Y, R, Fut: Future<Output = R>> Future for MiniGenFuture<'_, Y, R, Fut> {
            type Output = GeneratorStatus<Y, R>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                // The future was polled to completion before, so we should act similar to a
                // fused iterator and return that it's completed
                if self.generator.finished {
                    return Poll::Ready(GeneratorStatus::Completed);
                }
                let pinned = self.generator.future.as_mut();
                // We poll the future we're storing
                match pinned.poll(cx) {
                    // Future has returned, generator has completed.
                    Poll::Ready(v) => {
                        self.generator.finished = true;
                        Poll::Ready(GeneratorStatus::Returned(v))
                    }
                    // Yield or a random await
                    Poll::Pending => {
                        // We check here, because the future might yield for some reason other than
                        // stream.yield_value()
                        if let Some(v) = self.generator.receiver.try_recv() {
                            return Poll::Ready(GeneratorStatus::Yielded(v));
                        }
                        Poll::Pending
                    }
                }
            }
        }
    }

    pub struct MiniIter<Y, Fut: Future<Output = ()>>(pub(crate) MiniGen<Y, (), Fut>);

    impl<Y, Fut: Future<Output = ()>> Iterator for MiniIter<Y, Fut> {
        type Item = Y;
        fn next(&mut self) -> Option<Self::Item> {
            match self.0.resume() {
                GeneratorStatus::Yielded(it) => Some(it),
                GeneratorStatus::Returned(()) | GeneratorStatus::Completed => None,
            }
        }
    }

    impl<Y, Fut: Future<Output = ()>> Unpin for MiniIter<Y, Fut> {}

    #[must_use = "Must be awaited to produce a value!"]
    #[repr(transparent)]
    pub struct MiniIterFuture<'r, Y, Fut: Future<Output = ()>> {
        generator: &'r mut MiniIter<Y, Fut>,
    }

    impl<Y, Fut: Future<Output = ()>> MiniIter<Y, Fut> {
        pub fn resume_async(&mut self) -> MiniIterFuture<'_, Y, Fut> {
            MiniIterFuture {
                generator: &mut *self,
            }
        }
    }

    impl<Y, Fut: Future<Output = ()>> Future for MiniIterFuture<'_, Y, Fut> {
        type Output = Option<Y>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // The future was polled to completion before, so we should act similar to a
            // fused iterator and return that it's completed
            if self.generator.0.finished {
                return Poll::Ready(None);
            }
            let pinned = self.generator.0.future.as_mut();
            // We poll the future we're storing
            match pinned.poll(cx) {
                // Future has returned, generator has completed.
                Poll::Ready(()) => {
                    self.generator.0.finished = true;
                    Poll::Ready(None)
                }
                // Yield or a random await
                Poll::Pending => {
                    // We check here, because the future might yield for some reason other than
                    // stream.yield_value()
                    if let Some(v) = self.generator.0.receiver.try_recv() {
                        return Poll::Ready(Some(v));
                    }
                    Poll::Pending
                }
            }
        }
    }

    #[cfg(feature = "stream")]
    impl<Y, Fut: Future<Output = ()>> futures_core::Stream for MiniIter<Y, Fut> {
        type Item = Y;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let fut = self.resume_async();
            let fut = std::pin::pin!(fut);
            fut.poll(cx)
        }
    }

    #[derive(Clone, Debug, Copy, PartialEq, Eq)]
    pub enum GeneratorStatus<Y, R> {
        Yielded(Y),
        Returned(R),
        Completed,
    }
}

pub mod traits {
    use crate::{GeneratorStatus, MiniGen, MiniIter};

    #[cfg(feature = "send")]
    pub trait MaybeSend: Send {}
    #[cfg(feature = "send")]
    impl<T: Send> MaybeSend for T {}
    #[cfg(not(feature = "send"))]
    pub trait MaybeSend {}
    #[cfg(not(feature = "send"))]
    impl<T> MaybeSend for T {}
    pub trait MiniGenerator: MaybeSend {
        type Yield;
        type Return;
        fn resume(&mut self) -> GeneratorStatus<Self::Yield, Self::Return>;
        fn resume_async(
            &mut self,
        ) -> impl Future<Output = GeneratorStatus<Self::Yield, Self::Return>> + MaybeSend;
    }
    pub trait MiniIterator: MaybeSend {
        type Item;
        fn resume(&mut self) -> Option<Self::Item>;
        fn resume_async(&mut self) -> impl Future<Output = Option<Self::Item>> + MaybeSend;
    }

    impl<Y, R, Fut: Future<Output = R>> MiniGenerator for MiniGen<Y, R, Fut> {
        type Yield = Y;

        type Return = R;

        fn resume(&mut self) -> GeneratorStatus<Self::Yield, Self::Return> {
            self.resume()
        }

        fn resume_async(
            &mut self,
        ) -> impl Future<Output = GeneratorStatus<Self::Yield, Self::Return>> + MaybeSend {
            self.resume_async()
        }
    }

    impl<Y, Fut: Future<Output = ()>> MiniIterator for MiniIter<Y, Fut> {
        type Item = Y;

        fn resume(&mut self) -> Option<Self::Item> {
            self.next()
        }

        fn resume_async(&mut self) -> impl Future<Output = Option<Self::Item>> + MaybeSend {
            self.resume_async()
        }
    }
}

pub use generator::*;
pub use yielder::*;

pub fn generator<Y, R, Fut: Future<Output = R>, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniGen<Y, R, Fut> {
    #[cfg(not(feature = "send"))]
    let inner = std::rc::Rc::new(std::cell::Cell::new(None));
    #[cfg(feature = "send")]
    let inner = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (sender, receiver) = (Sender(inner.clone()), Receiver(inner));
    let stream = Yielder { sender };

    let future = func(stream);
    MiniGen {
        receiver,
        future: Box::pin(future),
        finished: false,
    }
}

pub fn iterator<Y, Fut: Future<Output = ()>, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniIter<Y, Fut> {
    MiniIter(generator(func))
}
