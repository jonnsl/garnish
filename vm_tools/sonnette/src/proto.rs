//! The protocol between `sonnette` (host) and `sonnette-agent` (guest).
//!
//! The host connects to the agent on vsock port [`AGENT_PORT`]. Every message
//! is one line of JSON, in both directions, at any time. The agent starts
//! with [`AgentMessage::Hello`], the host with [`HostMessage::Hello`]. After
//! that the agent sends notifications to show or close, and the host sends
//! the actions the user picked and the notifications that were closed.
//!
//! IDs are the guest's notification IDs. The host keeps its own IDs to
//! itself.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const AGENT_PORT: u32 = 8892;

/// Upper bound for a single message line: an image, base64-encoded, and
/// some text.
const MAX_LINE: u64 = 8 << 20;
/// Upper bound for icon files and pixel data.
pub const MAX_IMAGE: usize = 4 << 20;
/// Upper bound for the width and height of pixel data.
pub const MAX_IMAGE_SIDE: i32 = 1024;
/// Icon files are looked up at this size.
pub const ICON_SIZE: u32 = 128;

const MAX_APP_NAME: usize = 256;
const MAX_SUMMARY: usize = 1024;
const MAX_BODY: usize = 16 << 10;
const MAX_ACTIONS: usize = 16;
const MAX_SHORT: usize = 256;

/// The capabilities the host may pass on to the guest, if its server has
/// them. Not body-hyperlinks and body-images: links would open on the host,
/// image paths would be host paths. Not icon-multi: animations aren't
/// forwarded.
pub const SUPPORTED_CAPABILITIES: &[&str] = &[
    "actions",
    "action-icons",
    "body",
    "body-markup",
    "icon-static",
    "persistence",
    "sound",
];

/// What the agent claims before the host has said what its server can do.
pub const DEFAULT_CAPABILITIES: &[&str] = &[
    "actions",
    "body",
    "body-markup",
    "icon-static",
    "persistence",
];

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// `vm` is the VM's name, the guest's hostname, like carte's.
    Hello {
        vm: String,
    },
    Notify(Box<Notification>),
    Close {
        id: u32,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// The capabilities of the host's server, out of [`SUPPORTED_CAPABILITIES`].
    Hello { capabilities: Vec<String> },
    /// The user picked an action. "default" is a click on the notification.
    Action { id: u32, key: String },
    /// The notification is gone, with the spec's reason: 1 expired,
    /// 2 dismissed by the user, 3 closed by CloseNotification, 4 other.
    Closed { id: u32, reason: u32 },
}

pub const CLOSED_EXPIRED: u32 = 1;
pub const CLOSED_DISMISSED: u32 = 2;
pub const CLOSED_BY_CALL: u32 = 3;
pub const CLOSED_OTHER: u32 = 4;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    /// The desktop file ID of the sending application, without ".desktop":
    /// the desktop-entry hint, or else the application carte started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub hints: Hints,
    /// Milliseconds, -1 for the server's default, 0 for never.
    #[serde(default = "default_timeout")]
    pub expire_timeout: i32,
    /// The app_icon argument, read from the guest's files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_icon: Option<ImageFile>,
    /// The image-data or image-path hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
}

fn default_timeout() -> i32 {
    -1
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// The hints that mean the same on the host. image-data and image-path
/// become [`Notification::image`], desktop-entry [`Notification::app_id`].
/// The others are dropped: sound-file names a guest file, x and y are guest
/// screen positions.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Hints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_icons: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_sound: Option<bool>,
}

/// A PNG or SVG file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ImageFile {
    /// "png" or "svg".
    pub format: String,
    #[serde(with = "crate::base64::serde")]
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Image {
    /// The image-data hint: raw pixels, as in the spec's (iiibiiay).
    Pixels {
        width: i32,
        height: i32,
        rowstride: i32,
        has_alpha: bool,
        bits_per_sample: i32,
        channels: i32,
        #[serde(with = "crate::base64::serde")]
        data: Vec<u8>,
    },
    File(ImageFile),
}

impl ImageFile {
    pub fn is_valid(&self) -> bool {
        (self.format == "png" || self.format == "svg")
            && !self.data.is_empty()
            && self.data.len() <= MAX_IMAGE
    }
}

impl Image {
    pub fn is_valid(&self) -> bool {
        match self {
            Image::File(file) => file.is_valid(),
            &Image::Pixels {
                width,
                height,
                rowstride,
                has_alpha,
                bits_per_sample,
                channels,
                ref data,
            } => {
                // What GdkPixbuf, which most servers load it with, accepts.
                let side = 1..=MAX_IMAGE_SIDE;
                if !side.contains(&width)
                    || !side.contains(&height)
                    || bits_per_sample != 8
                    || channels != if has_alpha { 4 } else { 3 }
                    || data.len() > MAX_IMAGE
                {
                    return false;
                }
                // All small and positive now, except rowstride.
                let row = width as usize * channels as usize;
                let Ok(rowstride) = usize::try_from(rowstride) else {
                    return false;
                };
                // The last row may be cut short to its pixels.
                rowstride >= row
                    && rowstride <= MAX_IMAGE
                    && data.len() >= rowstride * (height as usize - 1) + row
            }
        }
    }
}

/// Cuts `s` down to at most `max` bytes, at a character boundary.
fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

impl Notification {
    /// Makes a notification from the guest safe to pass on: limits the
    /// lengths, drops invalid images and IDs, and strips links and images
    /// from the body.
    pub fn sanitize(&mut self) {
        truncate(&mut self.app_name, MAX_APP_NAME);
        truncate(&mut self.summary, MAX_SUMMARY);
        truncate(&mut self.body, MAX_BODY);
        self.body = crate::markup::strip_links_and_images(&self.body);
        self.actions.truncate(MAX_ACTIONS);
        for action in &mut self.actions {
            truncate(&mut action.key, MAX_SHORT);
            truncate(&mut action.label, MAX_SHORT);
        }
        if self
            .app_id
            .as_deref()
            .is_some_and(|id| !carte::proto::is_valid_id(id))
        {
            self.app_id = None;
        }
        if self.app_icon.as_ref().is_some_and(|i| !i.is_valid()) {
            self.app_icon = None;
        }
        if self.image.as_ref().is_some_and(|i| !i.is_valid()) {
            self.image = None;
        }
        for hint in [&mut self.hints.category, &mut self.hints.sound_name]
            .into_iter()
            .flatten()
        {
            truncate(hint, MAX_SHORT);
        }
        if self.hints.urgency.is_some_and(|u| u > 2) {
            self.hints.urgency = None;
        }
    }
}

pub async fn send<T: Serialize>(w: &mut (impl AsyncWrite + Unpin), msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line).await
}

/// Reads one message. `Ok(None)` means the other side closed the connection.
pub async fn recv<T: DeserializeOwned>(
    r: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    if r.take(MAX_LINE).read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long or cut off",
        ));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(width: i32, height: i32, rowstride: i32, len: usize) -> Image {
        Image::Pixels {
            width,
            height,
            rowstride,
            has_alpha: true,
            bits_per_sample: 8,
            channels: 4,
            data: vec![0; len],
        }
    }

    #[test]
    fn message_format() {
        let line = serde_json::to_string(&AgentMessage::Close { id: 3 }).unwrap();
        assert_eq!(line, r#"{"type":"close","id":3}"#);
        let msg: AgentMessage =
            serde_json::from_str(r#"{"type":"notify","id":1,"app_name":"a","summary":"s"}"#)
                .unwrap();
        let AgentMessage::Notify(n) = msg else {
            panic!("{msg:?}")
        };
        assert_eq!((n.id, n.expire_timeout, n.body.as_str()), (1, -1, ""));
        let msg: HostMessage =
            serde_json::from_str(r#"{"type":"action","id":1,"key":"default"}"#).unwrap();
        assert_eq!(
            msg,
            HostMessage::Action {
                id: 1,
                key: "default".into()
            }
        );
    }

    #[test]
    fn image_format() {
        let file = Image::File(ImageFile {
            format: "png".into(),
            data: b"abc".to_vec(),
        });
        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(json, r#"{"kind":"file","format":"png","data":"YWJj"}"#);
        assert_eq!(serde_json::from_str::<Image>(&json).unwrap(), file);
    }

    #[tokio::test]
    async fn round_trip() {
        let mut buf = Vec::new();
        let n = Notification {
            id: 7,
            summary: "hi".into(),
            image: Some(pixels(1, 1, 4, 4)),
            ..Default::default()
        };
        send(&mut buf, &AgentMessage::Notify(Box::new(n.clone())))
            .await
            .unwrap();
        send(&mut buf, &AgentMessage::Close { id: 7 })
            .await
            .unwrap();
        let mut r = &buf[..];
        assert_eq!(
            recv::<AgentMessage>(&mut r).await.unwrap(),
            Some(AgentMessage::Notify(Box::new(n)))
        );
        assert_eq!(
            recv::<AgentMessage>(&mut r).await.unwrap(),
            Some(AgentMessage::Close { id: 7 })
        );
        assert_eq!(recv::<AgentMessage>(&mut r).await.unwrap(), None);
        let mut cut = &b"{\"type\":"[..];
        assert!(recv::<AgentMessage>(&mut cut).await.is_err());
    }

    #[test]
    fn images() {
        assert!(pixels(2, 2, 8, 16).is_valid());
        // The last row without padding.
        assert!(pixels(2, 2, 12, 20).is_valid());
        assert!(!pixels(2, 2, 12, 19).is_valid());
        assert!(!pixels(2, 2, 4, 16).is_valid());
        assert!(!pixels(0, 2, 8, 16).is_valid());
        assert!(!pixels(2048, 1, 8192, 8192).is_valid());
        assert!(!pixels(-1, 2, 8, 16).is_valid());
        let mut wrong_channels = pixels(1, 1, 4, 4);
        if let Image::Pixels { channels, .. } = &mut wrong_channels {
            *channels = 3;
        }
        assert!(!wrong_channels.is_valid());
        assert!(!Image::File(ImageFile {
            format: "xpm".into(),
            data: b"x".to_vec()
        })
        .is_valid());
        assert!(!Image::File(ImageFile {
            format: "png".into(),
            data: vec![0; MAX_IMAGE + 1]
        })
        .is_valid());
    }

    #[test]
    fn sanitize() {
        let mut n = Notification {
            app_name: "é".repeat(200),
            app_id: Some("../evil".into()),
            body: r#"<b>x</b> <a href="file:///">y</a>"#.into(),
            actions: (0..20)
                .map(|i| Action {
                    key: i.to_string(),
                    label: "l".into(),
                })
                .collect(),
            hints: Hints {
                urgency: Some(9),
                ..Default::default()
            },
            image: Some(pixels(2, 2, 4, 4)),
            ..Default::default()
        };
        n.sanitize();
        assert_eq!(n.app_name.len(), 256);
        assert_eq!(n.app_id, None);
        assert_eq!(n.body, "<b>x</b> y");
        assert_eq!(n.actions.len(), MAX_ACTIONS);
        assert_eq!(n.hints.urgency, None);
        assert_eq!(n.image, None);
    }
}
