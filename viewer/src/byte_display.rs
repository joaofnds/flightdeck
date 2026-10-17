pub(crate) fn append(text: &mut String, bytes: &[u8]) {
    for chunk in bytes.utf8_chunks() {
        for character in chunk.valid().chars() {
            match character {
                '\\' => text.push_str("\\\\"),
                character if character.is_control() => {
                    let mut encoded = [0; 4];
                    for byte in character.encode_utf8(&mut encoded).bytes() {
                        append_escape(text, byte);
                    }
                }
                character => text.push(character),
            }
        }

        for byte in chunk.invalid() {
            append_escape(text, *byte);
        }
    }
}

fn append_escape(text: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    text.push_str("\\x");
    text.push(char::from(HEX[usize::from(byte >> 4)]));
    text.push(char::from(HEX[usize::from(byte & 15)]));
}
