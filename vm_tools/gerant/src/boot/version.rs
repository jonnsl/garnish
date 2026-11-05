//! Version comparison as systemd-boot does it (strverscmp_improved in
//! systemd's string-util-fundamental.c), to pick the newest kernel the
//! way the guest's boot loader would.

use std::cmp::Ordering;

fn valid(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"~-^.".contains(&c)
}

/// Compares two versions: `Greater` when `a` is newer.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    // Every turn of the loop that doesn't return consumes something, at
    // the latest the turn after; this bounds it all the same.
    for _ in 0..2 * (a.len() + b.len()) + 2 {
        while a.first().is_some_and(|&c| !valid(c)) {
            a = &a[1..];
        }
        while b.first().is_some_and(|&c| !valid(c)) {
            b = &b[1..];
        }

        // '~' marks a pre-release (123~rc1), the oldest; then whichever
        // has more segments is newer. '-' separates version and release,
        // '^' marks a patched release, '.' a point release: a segment with
        // one of those is older than one without, and they rank in that
        // order ('-' oldest).
        for sep in [b'~', 0, b'-', b'^', b'.'] {
            if sep == 0 {
                if a.is_empty() || b.is_empty() {
                    return a.first().cmp(&b.first());
                }
                continue;
            }
            let (fa, fb) = (a.first() == Some(&sep), b.first() == Some(&sep));
            if fa || fb {
                let r = (!fa).cmp(&!fb);
                if r != Ordering::Equal {
                    return r;
                }
                a = &a[1..];
                b = &b[1..];
            }
        }

        let digit = |s: &[u8]| s.first().is_some_and(u8::is_ascii_digit);
        if digit(a) || digit(b) {
            let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
            let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
            // A number is newer than nothing.
            let r = (na > 0).cmp(&(nb > 0));
            if r != Ordering::Equal {
                return r;
            }
            let zeros = |s: &[u8]| s.iter().take_while(|&&c| c == b'0').count();
            let (da, db) = (&a[zeros(&a[..na])..na], &b[zeros(&b[..nb])..nb]);
            let r = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
            if r != Ordering::Equal {
                return r;
            }
            a = &a[na..];
            b = &b[nb..];
        } else {
            let na = a.iter().take_while(|c| c.is_ascii_alphabetic()).count();
            let nb = b.iter().take_while(|c| c.is_ascii_alphabetic()).count();
            let n = na.min(nb);
            let r = a[..n].cmp(&b[..n]).then(na.cmp(&nb));
            if r != Ordering::Equal {
                return r;
            }
            a = &a[na..];
            b = &b[nb..];
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test]
    fn like_systemd() {
        // From systemd's test-string-util.c.
        let ordered = [
            "~1",
            "",
            "ab",
            "abb",
            "abc",
            "0001",
            "002",
            "12",
            "122",
            "122.9",
            "123~rc1",
            "123",
            "123-a",
            "123-a.1",
            "123-a1",
            "123-a1.1",
            "123-3",
            "123-3.1",
            "123^patch1",
            "123^1",
            "123.a-1",
            "123.1-1",
            "123a-1",
            "124",
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(compare(a, b), i.cmp(&j), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn corner_cases() {
        let newer = [
            ("123.45-67.88", "123.45-67.89"),
            ("123.45-67.89", "123.45-67.89a"),
            ("123.45-67.ab", "123.45-67.89"),
            ("123.45-67", "123.45-67.89"),
            ("123-99.99", "123.45-67.89"),
            ("123~rc1-99.99", "123^aa2-67.89"),
            ("123^45-67.89", "123.45-67.89"),
            ("123^aa2-67.89", "123aa2-67.89"),
            ("123.aa2-67.89", "123aa2-67.89"),
        ];
        for (old, new) in newer {
            assert_eq!(compare(old, new), Less, "{old} vs {new}");
            assert_eq!(compare(new, old), Greater, "{new} vs {old}");
        }
        assert_eq!(compare("123_aa2-67.89", "123aa+2-67.89"), Equal);
        assert_eq!(compare("123.", "123"), Greater);
        assert_eq!(compare("12_3", "12.3"), Greater);
        assert_eq!(compare("123..0", "123.0"), Less);
        assert_eq!(compare("_0_", "0"), Equal);
        assert_eq!(compare("", "_"), Equal);
    }

    #[test]
    fn kernels() {
        assert_eq!(compare("7.2.14-arch1-1", "7.2.9-arch1-1"), Greater);
        assert_eq!(compare("7.2.14-arch1-1", "7.2.14-arch2-1"), Less);
        assert_eq!(
            compare("6.12.48+deb13-amd64", "6.12.43+deb13-amd64"),
            Greater
        );
        assert_eq!(
            compare("6.17.1-300.fc44.x86_64", "6.17.1-300.fc44.x86_64"),
            Equal
        );
        assert_eq!(compare("7.0.0-rc1", "7.0.0"), Greater);
        assert_eq!(compare("7.0.0~rc1", "7.0.0"), Less);
    }

    #[test]
    fn terminates() {
        for (a, b) in [
            ("..", ".."),
            ("-.-", "-.-"),
            ("~~", "~"),
            ("^.~", "^.~"),
            ("1..", "1.-"),
        ] {
            compare(a, b);
            compare(b, a);
        }
    }
}
