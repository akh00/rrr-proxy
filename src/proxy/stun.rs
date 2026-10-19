//
//  Reads the local ICE ufrag out of a STUN binding request (RFC 5389). This decides which
//  target a datagram on a pooled socket is routed to, so every byte is untrusted: anything
//  that is not a well-formed binding request with a USERNAME yields None.
//

const HEADER_LEN: usize = 20;
const ATTR_HEADER_LEN: usize = 4;
const BINDING_REQUEST: u16 = 0x0001;
const ATTR_USERNAME: u16 = 0x0006;
const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

/// Returns the local ufrag, i.e. the part of the STUN USERNAME before the `:`.
#[inline]
pub fn local_ufrag(packet: &[u8]) -> Option<&str> {
    // cheap header checks first, so media traffic is rejected without walking attributes
    if packet.len() < HEADER_LEN || packet[4..8] != MAGIC_COOKIE {
        return None;
    }
    if u16::from_be_bytes([packet[0], packet[1]]) != BINDING_REQUEST {
        return None;
    }
    let msg_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if msg_len & 3 != 0 || HEADER_LEN + msg_len != packet.len() {
        return None;
    }

    let mut attrs = &packet[HEADER_LEN..];
    while attrs.len() >= ATTR_HEADER_LEN {
        let attr_type = u16::from_be_bytes([attrs[0], attrs[1]]);
        let attr_len = u16::from_be_bytes([attrs[2], attrs[3]]) as usize;
        let value = attrs.get(ATTR_HEADER_LEN..ATTR_HEADER_LEN + attr_len)?;
        if attr_type == ATTR_USERNAME {
            let ufrag = value.split(|b| *b == b':').next()?;
            return match std::str::from_utf8(ufrag) {
                Ok(ufrag) if !ufrag.is_empty() => Some(ufrag),
                _ => None,
            };
        }
        let padded_len = (attr_len + 3) & !3;
        attrs = attrs.get(ATTR_HEADER_LEN + padded_len..)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::local_ufrag;

    fn binding_request(username: &str) -> Vec<u8> {
        let username = username.as_bytes();
        let padded_len = (username.len() + 3) & !3;
        let attrs_len = 4 + padded_len;
        let mut packet = Vec::with_capacity(20 + attrs_len);
        packet.extend_from_slice(&0x0001u16.to_be_bytes());
        packet.extend_from_slice(&(attrs_len as u16).to_be_bytes());
        packet.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        packet.extend_from_slice(&[7; 12]);
        packet.extend_from_slice(&0x0006u16.to_be_bytes());
        packet.extend_from_slice(&(username.len() as u16).to_be_bytes());
        packet.extend_from_slice(username);
        packet.resize(20 + attrs_len, 0);
        packet
    }

    #[test]
    fn extracts_local_ufrag_from_binding_request() {
        assert_eq!(
            local_ufrag(&binding_request("myUfrag:remote")),
            Some("myUfrag")
        );
    }

    #[test]
    fn extracts_whole_username_without_colon() {
        assert_eq!(local_ufrag(&binding_request("myUfrag")), Some("myUfrag"));
    }

    #[test]
    fn skips_attributes_preceding_username() {
        let mut packet = binding_request("myUfrag:remote");
        // prepend a 5 byte (padded to 8) PRIORITY-like attribute before USERNAME
        let extra = [0x00, 0x24, 0x00, 0x05, 1, 2, 3, 4, 5, 0, 0, 0];
        packet.splice(20..20, extra);
        let msg_len = (packet.len() - 20) as u16;
        packet[2..4].copy_from_slice(&msg_len.to_be_bytes());
        assert_eq!(local_ufrag(&packet), Some("myUfrag"));
    }

    #[test]
    fn reads_ufrag_as_utf8() {
        assert_eq!(local_ufrag(&binding_request("ufräg:remote")), Some("ufräg"));
    }

    #[test]
    fn rejects_empty_and_short_datagrams() {
        assert_eq!(local_ufrag(&[]), None);
        assert_eq!(local_ufrag(&[0; 19]), None);
    }

    #[test]
    fn rejects_packet_without_magic_cookie() {
        let mut packet = binding_request("myUfrag:remote");
        packet[4] = 0;
        assert_eq!(local_ufrag(&packet), None);
    }

    #[test]
    fn rejects_plain_media_payload() {
        let mut rtp = [0u8; 64];
        rtp[0] = 0x80;
        assert_eq!(local_ufrag(&rtp), None);
    }

    #[test]
    fn rejects_non_binding_request() {
        let mut packet = binding_request("myUfrag:remote");
        packet[0] = 0x01;
        packet[1] = 0x01; // binding success response
        assert_eq!(local_ufrag(&packet), None);
    }

    #[test]
    fn rejects_binding_request_without_username() {
        let mut packet = binding_request("myUfrag:remote");
        packet.truncate(20);
        packet[2..4].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(local_ufrag(&packet), None);
    }

    #[test]
    fn rejects_truncated_attribute_section() {
        let mut packet = binding_request("myUfrag:remote");
        packet.truncate(packet.len() - 4);
        assert_eq!(local_ufrag(&packet), None);
    }

    #[test]
    fn rejects_attribute_overrunning_message() {
        let mut packet = binding_request("myUfrag:remote");
        packet[22..24].copy_from_slice(&0x0100u16.to_be_bytes());
        assert_eq!(local_ufrag(&packet), None);
    }

    #[test]
    fn rejects_empty_or_invalid_ufrag() {
        assert_eq!(local_ufrag(&binding_request(":remote")), None);
        let mut packet = binding_request("ab:remote");
        packet[24] = 0xFF; // not valid UTF-8
        assert_eq!(local_ufrag(&packet), None);
    }
}
