use std::{io, os::fd::OwnedFd, str};

use pazari_types::{Clock, Interval};
use rustix::net::{
    AddressFamily, RecvFlags, SocketFlags, SocketType, bind,
    netlink::{KOBJECT_UEVENT, SocketAddrNetlink},
    recv, socket_with,
};
use tokio::{io::unix::AsyncFd, time::timeout};

use crate::{
    engine::Event,
    runtime::{error::RuntimeError, topology::Topology},
};

/// The kernel's own uevent multicast group.
const KERNEL_GROUP: u32 = 1;

/// `UEVENT_BUFFER_SIZE`: the kernel will not multicast a longer payload.
const PAYLOAD: usize = 2048;

/// What a `SUBSYSTEM=memory` uevent announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotplug {
    Add,
    Remove,
    Online,
    Offline,
}

#[must_use]
pub fn parse(payload: &[u8]) -> Option<Hotplug> {
    let mut subsystem = None;
    let mut action = None;

    for field in payload.split(|byte| *byte == 0).skip(1) {
        let Ok(field) = str::from_utf8(field) else {
            continue;
        };

        let Some((key, value)) = field.split_once('=') else {
            continue;
        };

        match key {
            "SUBSYSTEM" => subsystem = Some(value),
            "ACTION" => action = Some(value),
            _ => {}
        }
    }

    if subsystem? != "memory" {
        return None;
    }

    match action? {
        "add" => Some(Hotplug::Add),
        "remove" => Some(Hotplug::Remove),
        "online" => Some(Hotplug::Online),
        "offline" => Some(Hotplug::Offline),
        _ => None,
    }
}

/// Watches the kobject uevent socket for the guest's memory changing size.
#[derive(Debug)]
pub struct HotplugListener {
    socket: AsyncFd<OwnedFd>,
    topology: Topology,
    debounce: Interval,
    clock: Clock,
}

impl HotplugListener {
    pub fn bind(
        topology: Topology,
        debounce: Interval,
        clock: Clock,
    ) -> Result<Self, RuntimeError> {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            Some(KOBJECT_UEVENT),
        )
        .map_err(|source| RuntimeError::Netlink {
            what: "open",
            source: source.into(),
        })?;

        bind(&socket, &SocketAddrNetlink::new(0, KERNEL_GROUP)).map_err(|source| {
            RuntimeError::Netlink {
                what: "bind",
                source: source.into(),
            }
        })?;

        let socket = AsyncFd::new(socket).map_err(|source| RuntimeError::Netlink {
            what: "watch",
            source,
        })?;

        Ok(Self {
            socket,
            topology,
            debounce,
            clock,
        })
    }

    pub async fn changed(&self) -> Result<Event, RuntimeError> {
        let mut payload = [0_u8; PAYLOAD];

        loop {
            let len = self.recv(&mut payload).await?;

            if parse(&payload[..len]).is_some() {
                break;
            }
        }

        while let Ok(result) = timeout(self.debounce.get(), self.recv(&mut payload)).await {
            result?;
        }

        Ok(Event::PluggedChanged {
            now: self.clock.now(),
            plugged: self.topology.plugged().await?,
        })
    }

    async fn recv(&self, payload: &mut [u8]) -> Result<usize, RuntimeError> {
        loop {
            let mut ready =
                self.socket
                    .readable()
                    .await
                    .map_err(|source| RuntimeError::Netlink {
                        what: "wait on",
                        source,
                    })?;

            let read = ready.try_io(|socket| {
                recv(socket.get_ref(), &mut *payload, RecvFlags::empty())
                    .map(|(len, _)| len)
                    .map_err(io::Error::from)
            });

            if let Ok(result) = read {
                return result.map_err(|source| RuntimeError::Netlink {
                    what: "read from",
                    source,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn payload(header: &str, fields: &[&str]) -> Vec<u8> {
        let mut bytes = header.as_bytes().to_vec();

        for field in fields {
            bytes.push(0);
            bytes.extend_from_slice(field.as_bytes());
        }

        bytes.push(0);
        bytes
    }

    fn memory(action: &str) -> Vec<u8> {
        payload(
            &format!("{action}@/devices/system/memory/memory42"),
            &[
                &format!("ACTION={action}"),
                "DEVPATH=/devices/system/memory/memory42",
                "SUBSYSTEM=memory",
                "SEQNUM=4831",
            ],
        )
    }

    #[test]
    fn every_memory_hotplug_is_recognised() {
        assert_eq!(parse(&memory("add")), Some(Hotplug::Add));
        assert_eq!(parse(&memory("remove")), Some(Hotplug::Remove));
        assert_eq!(parse(&memory("online")), Some(Hotplug::Online));
        assert_eq!(parse(&memory("offline")), Some(Hotplug::Offline));
    }

    #[test]
    fn another_subsystem_is_not_a_hotplug() {
        let block = payload(
            "add@/devices/pci0000:00/virtio2/block/vda",
            &[
                "ACTION=add",
                "DEVPATH=/devices/pci0000:00/virtio2/block/vda",
                "SUBSYSTEM=block",
                "DEVNAME=vda",
            ],
        );

        assert_eq!(parse(&block), None);
    }

    #[test]
    fn a_memory_event_that_moves_nothing_is_not_a_hotplug() {
        let change = payload(
            "change@/devices/system/memory/memory42",
            &[
                "ACTION=change",
                "SUBSYSTEM=memory",
                "DEVPATH=/devices/system/memory/memory42",
            ],
        );

        assert_eq!(parse(&change), None);
    }

    #[test]
    fn a_payload_without_the_properties_that_name_it_is_not_a_hotplug() {
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"add@/devices/system/memory/memory42\0"), None);
        assert_eq!(
            parse(&payload(
                "add@/devices/system/memory/memory42",
                &["ACTION=add", "DEVPATH=/devices/system/memory/memory42"],
            )),
            None
        );
        assert_eq!(
            parse(&payload(
                "add@/devices/system/memory/memory42",
                &["SUBSYSTEM=memory"],
            )),
            None
        );
    }

    #[tokio::test]
    async fn the_kernel_uevent_group_binds_where_it_can_be_bound() {
        let debounce = Interval::try_new(Duration::from_millis(10)).unwrap();

        match HotplugListener::bind(Topology::default(), debounce, Clock::start()) {
            Ok(listener) => assert_eq!(listener.debounce, debounce),
            Err(error) => eprintln!("skipped: {error}"),
        }
    }

    #[test]
    fn a_property_that_is_not_utf8_is_stepped_over() {
        let mut bytes = b"add@/devices/system/memory/memory42\0DEVNAME=".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\0ACTION=online\0SUBSYSTEM=memory\0");

        assert_eq!(parse(&bytes), Some(Hotplug::Online));
    }
}
