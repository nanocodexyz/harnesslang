pub struct Part {
    pub name: String,
    pub file_name: Option<String>,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

pub struct Form {
    pub boundary: String,
    pub body: Vec<u8>,
}

const CRLF: &[u8] = b"\r\n";
const BOUNDARY_PREFIX: &str = "hl-deploy-";

pub fn encode(parts: &[Part]) -> Form {
    let boundary = boundary_for(parts);
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(b"--");
        body.extend_from_slice(boundary.as_bytes());
        body.extend_from_slice(CRLF);
        let disposition = match &part.file_name {
            Some(file) => format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"",
                part.name, file
            ),
            None => format!("Content-Disposition: form-data; name=\"{}\"", part.name),
        };
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(CRLF);
        body.extend_from_slice(format!("Content-Type: {}", part.content_type).as_bytes());
        body.extend_from_slice(CRLF);
        body.extend_from_slice(CRLF);
        body.extend_from_slice(&part.bytes);
        body.extend_from_slice(CRLF);
    }
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"--");
    body.extend_from_slice(CRLF);
    Form { boundary, body }
}

fn boundary_for(parts: &[Part]) -> String {
    let mut digest_input = Vec::new();
    for part in parts {
        digest_input.extend_from_slice(part.name.as_bytes());
        digest_input.extend_from_slice(&part.bytes);
    }
    let boundary = format!("{BOUNDARY_PREFIX}{}", super::sha1::sha1_hex(&digest_input));
    for part in parts {
        assert!(
            !contains(&part.bytes, boundary.as_bytes()),
            "a part never holds the boundary derived from its own digest"
        );
        assert!(
            !part.name.contains('"'),
            "part names are ours and hold no quote"
        );
    }
    boundary
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}
