//! An anonymous listener in this process, so the client and server code runs without launchd.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;

use super::raw::{
    Active, Owned, Queue, XpcObject, xpc_connection_create, xpc_connection_create_from_endpoint,
    xpc_connection_set_target_queue, xpc_endpoint_create,
};
use super::{
    Authorization, Peer, Started, checked, cstring, deadline, link_message, listen, request, send,
    start,
};

pub(in crate::macos::helper) struct Anonymous {
    listener: Active,
    _queue: Queue,
}

impl Anonymous {
    /// Listens as `serve` does, for clients that meet `requirement` and hold `right` (which is
    /// not defined in the policy database: tests use rights that exist).
    pub(in crate::macos::helper) fn listen(
        requirement: &str,
        right: &str,
        on_peer: impl Fn(Peer) -> Started + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let requirement = checked(requirement)?;
        let right = cstring(right)?;
        let queue = Queue::new(c"app.dfdu.helper.test");
        // SAFETY: a NULL name makes an anonymous listener; `queue` is live. Ours, to `listen`.
        let listener = unsafe { xpc_connection_create(std::ptr::null(), queue.0) };
        let listener = listen(listener, requirement, right, Arc::new(on_peer), |_| {})?;
        Ok(Self {
            listener,
            _queue: queue,
        })
    }

    /// `connect`, to this listener.
    pub(in crate::macos::helper) fn connect(
        &self,
        requirement: &str,
        link: BorrowedFd<'_>,
        auth: &Authorization,
        timeout: Duration,
    ) -> io::Result<()> {
        let requirement = cstring(requirement)?;
        let form = auth.external_form()?;
        self.with_client(|conn, queue| {
            request(conn, queue, Some(&requirement), link, Some(&form), timeout)
        })
    }

    /// Sends `{link, auth}` alone, without the empty request first and without requiring
    /// anything of the listener: what a client of its own making could send.
    pub(in crate::macos::helper) fn send_link(
        &self,
        link: BorrowedFd<'_>,
        auth: Option<&[u8]>,
        timeout: Duration,
    ) -> io::Result<()> {
        self.with_client(|conn, queue| {
            let ignore: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(|_: XpcObject| {});
            let conn = start(conn, None, &ignore)?;
            send(&conn, queue, &link_message(link, auth)?, deadline(timeout))
        })
    }

    /// Runs `f` with a new connection to this listener (inactive, for `f` to own, or NULL)
    /// and its queue.
    fn with_client<T>(&self, f: impl FnOnce(XpcObject, &Queue) -> T) -> T {
        let queue = Queue::new(c"app.dfdu.helper.test.client");
        // SAFETY: the listener is an active anonymous listener; the endpoint is ours and
        // released when `endpoint` drops, after the connection made from it (which keeps
        // what it needs of it). The new connection is inactive, so its target queue may be
        // set; it goes to `f`.
        unsafe {
            let endpoint = Owned(xpc_endpoint_create(self.listener.0));
            let conn = xpc_connection_create_from_endpoint(endpoint.0);
            if !conn.is_null() {
                xpc_connection_set_target_queue(conn, queue.0);
            }
            f(conn, &queue)
        }
    }
}
