use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, RwLock},
};

pub struct Sync<T: Sized>(Arc<Mutex<T>>);

pub struct GuardSync<T: Sized> {
    t: Sync<T>,
    thread: std::thread::ThreadId,
}

impl<T> Sync<T>
where
    T: Sized,
{
    pub fn new(t: T) -> Self {
        Self(Arc::new(Mutex::new(t)))
    }

    pub fn lock<F, O>(&self, f: F) -> O
    where
        F: FnOnce(&T) -> O,
        O: Send,
    {
        let mut locked = self.0.lock().unwrap();
        f(locked.deref())
    }

    pub fn lock_mut<F, O>(&self, f: F) -> O
    where
        F: FnOnce(&mut T) -> O,
        O: Send,
    {
        let mut locked = self.0.lock().unwrap();
        f(locked.deref_mut())
    }

    pub fn chain<F, O>(self, f: F) -> Self
    where
        F: FnOnce(&T) -> O,
        O: Send,
    {
        drop(self.lock(f));
        self
    }

    pub fn chain_mut<F, O>(self, f: F) -> Self
    where
        F: FnOnce(&mut T) -> O,
        O: Send,
    {
        drop(self.lock_mut(f));
        self
    }

    pub fn chain_clone<F, O>(self, f: F) -> Self
    where
        F: FnOnce(GuardSync<T>, &T) -> O,
        O: Send,
    {
        let t = self.0.lock().unwrap();
        f(GuardSync::new(self.clone()), t.deref());
        drop(t);
        self
    }

    pub fn chain_clone_mut<F, O>(self, f: F) -> Self
    where
        F: FnOnce(GuardSync<T>, &mut T) -> O,
        O: Send,
    {
        let mut t = self.0.lock().unwrap();
        f(GuardSync::new(self.clone()), t.deref_mut());
        drop(t);
        self
    }
}

impl<T> GuardSync<T>
where
    T: Sized,
{
    fn new(t: Sync<T>) -> Self {
        Self {
            t: t,
            thread: std::thread::current().id(),
        }
    }

    pub fn get(self) -> std::io::Result<Sync<T>> {
        Ok(self.t)
    }
}

impl<T> Deref for GuardSync<T>
where
    T: Sized,
{
    type Target = Sync<T>;
    fn deref(&self) -> &Self::Target {
        &self.t
    }
}

impl<T> Clone for Sync<T>
where
    T: Sized,
{
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
