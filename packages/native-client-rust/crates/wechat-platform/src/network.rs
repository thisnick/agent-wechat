//! Local interface enumeration without DNS, route probes or external traffic.
use std::{io, net::Ipv4Addr};

pub fn local_ipv4_addresses() -> io::Result<Vec<Ipv4Addr>> {
    let mut addresses = interfaces()?;
    addresses.retain(|ip| usable(*ip));
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.len() > 64 {
        return Err(io::Error::other("Too many local IPv4 addresses"));
    }
    Ok(addresses)
}

fn usable(ip: Ipv4Addr) -> bool {
    !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()
        && !ip.is_broadcast() && ip.octets()[0] != 0
}

#[cfg(unix)]
fn interfaces() -> io::Result<Vec<Ipv4Addr>> {
    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let mut head = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let guard = Addresses(head);
    let mut cursor = guard.0;
    let mut out = Vec::new();
    let mut count = 0;
    while !cursor.is_null() {
        count += 1;
        if count > 4096 {
            return Err(io::Error::other("Too many local interfaces"));
        }
        // getifaddrs owns this linked list and all sockaddr storage until free.
        let entry = unsafe { &*cursor };
        if entry.ifa_flags & libc::IFF_UP as u32 != 0
            && entry.ifa_flags & libc::IFF_LOOPBACK as u32 == 0
            && !entry.ifa_addr.is_null()
            && unsafe { (*entry.ifa_addr).sa_family as i32 } == libc::AF_INET
        {
            let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
            out.push(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()));
        }
        cursor = entry.ifa_next;
    }
    Ok(out)
}

#[cfg(windows)]
fn interfaces() -> io::Result<Vec<Ipv4Addr>> {
    use std::{ffi::c_void, mem::{offset_of, size_of}, net::UdpSocket, os::windows::io::AsRawSocket};
    #[repr(C)]
    struct SocketAddress { address: *const u8, length: i32 }
    #[repr(C)]
    struct AddressList { count: i32, first: SocketAddress }
    #[link(name = "ws2_32")]
    extern "system" {
        fn WSAIoctl(socket: usize, code: u32, input: *const c_void, input_len: u32,
            output: *mut c_void, output_len: u32, returned: *mut u32,
            overlapped: *mut c_void, completion: *mut c_void) -> i32;
        fn WSAGetLastError() -> i32;
    }
    // SIO_ADDRESS_LIST_QUERY on an IPv4 socket returns local bindings only.
    // std initializes Winsock; no UDP packet is sent and no peer is connected.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    let mut storage = vec![0usize; 65536 / size_of::<usize>()];
    let mut returned = 0;
    if unsafe { WSAIoctl(socket.as_raw_socket() as usize, 0x48000016,
        std::ptr::null(), 0, storage.as_mut_ptr().cast(), 65536, &mut returned,
        std::ptr::null_mut(), std::ptr::null_mut()) } != 0
    {
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    let base = storage.as_ptr() as usize;
    let end = base + returned as usize;
    let offset = offset_of!(AddressList, first);
    if returned as usize > 65536 || (returned as usize) < offset {
        return Err(io::Error::other("Invalid local address list"));
    }
    let count = unsafe { *storage.as_ptr().cast::<i32>() };
    if !(0..=4096).contains(&count)
        || offset + count as usize * size_of::<SocketAddress>() > returned as usize
    {
        return Err(io::Error::other("Invalid local address count"));
    }
    let first = unsafe { storage.as_ptr().cast::<u8>().add(offset).cast::<SocketAddress>() };
    let mut out = Vec::new();
    for n in 0..count as usize {
        let entry = unsafe { &*first.add(n) };
        let ptr = entry.address as usize;
        if entry.length < 8 || ptr < base || ptr.checked_add(8).is_none_or(|v| v > end) {
            return Err(io::Error::other("Invalid local address"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(entry.address, 8) };
        if u16::from_ne_bytes([bytes[0], bytes[1]]) == 2 {
            out.push(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertised_addresses_are_unicast_interface_addresses() {
        for ip in ["0.0.0.0", "0.1.2.3", "127.0.0.1", "224.0.0.1", "255.255.255.255"] {
            assert!(!usable(ip.parse().unwrap()));
        }
        for ip in ["192.168.1.2", "10.0.0.2", "172.20.0.2", "100.64.0.2", "169.254.1.2"] {
            assert!(usable(ip.parse().unwrap()));
        }
    }
    #[test]
    fn enumeration_is_bounded_sorted_and_filtered() {
        let addresses = local_ipv4_addresses().unwrap();
        assert!(addresses.len() <= 64);
        assert!(addresses.iter().all(|ip| usable(*ip)));
        assert!(addresses.windows(2).all(|w| w[0] < w[1]));
    }
}
