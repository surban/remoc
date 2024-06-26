use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt,
    hash::Hash,
    mem,
    ops::Deref,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

struct PortAllocatorInner {
    used: HashSet<u32>,
    limit: u32,
    notify_tx: Vec<oneshot::Sender<()>>,
}

impl PortAllocatorInner {
    fn is_available(&self) -> bool {
        self.used.len() <= self.limit as usize
    }

    fn try_allocate(&mut self, this: Arc<Mutex<PortAllocatorInner>>) -> Option<PortNumber> {
        if self.is_available() {
            let number = loop {
                let cand = rand::random();
                if !self.used.contains(&cand) {
                    break cand;
                }
            };

            self.used.insert(number);
            Some(PortNumber { number, allocator: this })
        } else {
            None
        }
    }
}

/// Local port number allocator.
///
/// State is shared between clones of this type.
#[derive(Clone)]
pub struct PortAllocator(Arc<Mutex<PortAllocatorInner>>);

impl fmt::Debug for PortAllocator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let inner = self.0.lock().unwrap();
        f.debug_struct("PortAllocator").field("used", &inner.used.len()).field("limit", &inner.limit).finish()
    }
}

impl PortAllocator {
    /// Creates a new port number allocator.
    pub(crate) fn new(limit: u32) -> PortAllocator {
        let inner = PortAllocatorInner { used: HashSet::new(), limit, notify_tx: Vec::new() };
        PortAllocator(Arc::new(Mutex::new(inner)))
    }

    /// Allocates a local port number.
    ///
    /// Port numbers are allocated randomly.
    /// If all ports are currently in use, this waits for a port number to become available.
    pub async fn allocate(&self) -> PortNumber {
        loop {
            let rx = {
                let mut inner = self.0.lock().unwrap();
                match inner.try_allocate(self.0.clone()) {
                    Some(number) => return number,
                    None => {
                        let (tx, rx) = oneshot::channel();
                        inner.notify_tx.push(tx);
                        rx
                    }
                }
            };

            let _ = rx.await;
        }
    }

    /// Tries to allocate a local port number.
    ///
    /// If all port are currently in use, this returns [None].
    pub fn try_allocate(&self) -> Option<PortNumber> {
        let mut inner = self.0.lock().unwrap();
        inner.try_allocate(self.0.clone())
    }
}

/// An allocated local port number.
///
/// When this is dropped, the allocated is automatically released.
pub struct PortNumber {
    number: u32,
    allocator: Arc<Mutex<PortAllocatorInner>>,
}

impl fmt::Debug for PortNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.number)
    }
}

impl fmt::Display for PortNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.number)
    }
}

impl Deref for PortNumber {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.number
    }
}

impl PartialEq for PortNumber {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for PortNumber {}

impl PartialOrd for PortNumber {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PortNumber {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.number.cmp(&other.number)
    }
}

impl Hash for PortNumber {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl Borrow<u32> for PortNumber {
    fn borrow(&self) -> &u32 {
        &self.number
    }
}

impl Drop for PortNumber {
    fn drop(&mut self) {
        let notify_tx = {
            let mut inner = self.allocator.lock().unwrap();
            inner.used.remove(&self.number);
            mem::take(&mut inner.notify_tx)
        };

        for tx in notify_tx {
            let _ = tx.send(());
        }
    }
}

/// A port connection request by the local endpoint.
///
/// The id can be set freely by the user.
/// It is initialized to the [port number](Self::port).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortReq {
    /// The allocated, local port number.
    pub port: PortNumber,
    /// A user-specified id.
    pub id: u32,
}

impl From<PortNumber> for PortReq {
    /// Create a new port connection request with [`id`](Self::id) set to
    /// the [port number](Self::port).
    fn from(port: PortNumber) -> Self {
        Self { id: port.number, port }
    }
}

impl From<PortReq> for PortNumber {
    fn from(req: PortReq) -> Self {
        req.port
    }
}

impl PortReq {
    /// Create a new port connection request with [`id`](Self::id) set to
    /// the [port number](Self::port).
    pub fn new(port: PortNumber) -> Self {
        Self::from(port)
    }

    /// Sets the id to the specified value.
    pub fn with_id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }
}
