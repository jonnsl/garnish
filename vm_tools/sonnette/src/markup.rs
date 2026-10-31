//! Body markup from the guest. The notification spec allows `<b>`, `<i>`,
//! `<u>`, `<a href>` and `<img src>`. Links would open on the host, and image
//! paths would be host paths, so the host drops those two tags (keeping a
//! link's text) and leaves the rest to its notification server.

/// Removes `<a>`, `</a>` and `<img>` tags from `body`.
pub fn strip_links_and_images(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tag = &rest[start..];
        let Some(end) = tag.find('>') else {
            // Not a tag; the server escapes or rejects it.
            out.push_str(tag);
            return out;
        };
        let name: String = tag[1..end]
            .trim_start_matches('/')
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !name.eq_ignore_ascii_case("a") && !name.eq_ignore_ascii_case("img") {
            out.push_str(&tag[..=end]);
        }
        rest = &tag[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips() {
        assert_eq!(strip_links_and_images("plain"), "plain");
        assert_eq!(
            strip_links_and_images("<b>bold</b> <i>it</i>"),
            "<b>bold</b> <i>it</i>"
        );
        assert_eq!(
            strip_links_and_images(r#"see <a href="https://x">this</a>!"#),
            "see this!"
        );
        assert_eq!(strip_links_and_images(r#"<IMG src="/etc/passwd"/>x"#), "x");
        assert_eq!(strip_links_and_images("< A HREF='x'>y</ a >"), "y");
        assert_eq!(strip_links_and_images("<abbr>k</abbr>"), "<abbr>k</abbr>");
        assert_eq!(strip_links_and_images("1 < 2"), "1 < 2");
    }
}
