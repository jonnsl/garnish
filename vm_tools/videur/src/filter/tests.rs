use std::path::Path;

use super::*;

const REGISTRY: u32 = 2;
const SEAT: u32 = 3;
const MANAGER: u32 = 4;
const DEVICE: u32 = 5;
const SOURCE: u32 = 6;
const OFFER: u32 = 0xff00_0001;

fn protocols() -> Rc<Protocols> {
    Rc::new(
        Protocols::load(&[
            Path::new("/usr/share/wayland"),
            Path::new("/usr/share/wayland-protocols/unstable/primary-selection"),
        ])
        .unwrap(),
    )
}

type F = u32;

fn req(f: &mut Filter<F>, conn: ConnId, msg: Vec<u8>, fds: Vec<F>) -> Result<Vec<Out<F>>, String> {
    f.handle(conn, true, msg, fds)
}

fn ev(f: &mut Filter<F>, conn: ConnId, msg: Vec<u8>) -> Vec<Out<F>> {
    f.handle(conn, false, msg, Vec::new()).unwrap()
}

/// The messages that reach the guest, and the compositor.
type Sent = Vec<(ConnId, Vec<u8>, Vec<F>)>;

fn split(out: Vec<Out<F>>) -> (Sent, Sent) {
    let (mut client, mut server) = (Vec::new(), Vec::new());
    for o in out {
        match o {
            Out::ToClient(c, m, f) => client.push((c, m, f)),
            Out::ToServer(c, m, f) => server.push((c, m, f)),
        }
    }
    (client, server)
}

fn global(name: u32, iface: &str, version: u32) -> Vec<u8> {
    Builder::new(REGISTRY, 0)
        .uint(name)
        .string(iface.as_bytes())
        .uint(version)
        .build()
}

fn bind(name: u32, iface: &str, version: u32, id: u32) -> Vec<u8> {
    Builder::new(REGISTRY, 0)
        .uint(name)
        .string(iface.as_bytes())
        .uint(version)
        .uint(id)
        .build()
}

/// A guest with a seat, a data device and a data source, on `conn`.
fn setup(f: &mut Filter<F>, conn: ConnId) {
    f.open(conn);
    req(f, conn, Builder::new(1, 1).uint(REGISTRY).build(), vec![]).unwrap();
    ev(f, conn, global(1, "wl_seat", 7));
    ev(f, conn, global(2, "wl_data_device_manager", 3));
    req(f, conn, bind(1, "wl_seat", 7, SEAT), vec![]).unwrap();
    req(
        f,
        conn,
        bind(2, "wl_data_device_manager", 3, MANAGER),
        vec![],
    )
    .unwrap();
    req(
        f,
        conn,
        Builder::new(MANAGER, 1).uint(DEVICE).uint(SEAT).build(),
        vec![],
    )
    .unwrap();
    req(
        f,
        conn,
        Builder::new(MANAGER, 0).uint(SOURCE).build(),
        vec![],
    )
    .unwrap();
}

fn filter(policy: &str) -> Filter<F> {
    let mut f = Filter::new(protocols(), Policy::parse(policy).unwrap()).unwrap();
    setup(&mut f, 1);
    f
}

/// The compositor offers `offer` as the selection.
fn offer_selection(f: &mut Filter<F>, conn: ConnId, offer: u32) -> Vec<Out<F>> {
    let mut out = ev(f, conn, Builder::new(DEVICE, 0).uint(offer).build());
    out.extend(ev(
        f,
        conn,
        Builder::new(offer, 0).string(b"text/plain").build(),
    ));
    out.extend(ev(f, conn, Builder::new(DEVICE, 5).uint(offer).build()));
    out
}

fn receive(offer: u32) -> Vec<u8> {
    Builder::new(offer, 1).string(b"text/plain").build()
}

fn selection_arg(msg: &[u8]) -> u32 {
    u32::from_ne_bytes(msg[8..12].try_into().unwrap())
}

#[test]
fn hides_unknown_and_data_control_globals() {
    let mut f = filter("both");
    assert!(ev(&mut f, 1, global(9, "zwlr_data_control_manager_v1", 2)).is_empty());
    assert!(ev(&mut f, 1, global(10, "some_private_thing", 1)).is_empty());
    assert!(
        req(
            &mut f,
            1,
            bind(9, "zwlr_data_control_manager_v1", 2, 20),
            vec![]
        )
        .is_err()
    );
    // Not even under the name of one it was offered.
    let mut f = filter("both");
    assert!(
        req(
            &mut f,
            1,
            bind(1, "zwlr_data_control_manager_v1", 2, 20),
            vec![]
        )
        .is_err()
    );
    // Removing a hidden global isn't passed on either.
    let mut f = filter("both");
    ev(&mut f, 1, global(9, "zwlr_data_control_manager_v1", 2));
    assert!(ev(&mut f, 1, Builder::new(REGISTRY, 1).uint(9).build()).is_empty());
}

#[test]
fn caps_versions_at_the_known_protocol() {
    let mut f = filter("both");
    let (client, _) = split(ev(&mut f, 1, global(9, "wl_compositor", 999)));
    let version = u32::from_ne_bytes(client[0].1[client[0].1.len() - 4..].try_into().unwrap());
    let known = f
        .protocols
        .get(f.protocols.find("wl_compositor").unwrap())
        .version;
    assert_eq!(version, known);
    assert!(req(&mut f, 1, bind(9, "wl_compositor", 999, 20), vec![]).is_err());
    assert!(req(&mut f, 1, bind(9, "wl_compositor", known, 20), vec![]).is_ok());
}

#[test]
fn host_copy_is_kept_from_the_guest() {
    for policy in ["none", "to-host"] {
        let mut f = filter(policy);
        let (client, server) = split(offer_selection(&mut f, 1, OFFER));
        // Only the selection, emptied; the offer is given back.
        assert_eq!(client.len(), 1);
        assert_eq!(selection_arg(&client[0].1), 0);
        assert_eq!(server, vec![(1, Builder::new(OFFER, 2).build(), vec![])]);
        // The guest can't use it anyway.
        assert!(req(&mut f, 1, receive(OFFER), vec![7]).is_err());
        // And it hears nothing more of it.
        assert!(ev(&mut f, 1, Builder::new(OFFER, 1).uint(1).build()).is_empty());
    }
}

#[test]
fn guest_cant_use_an_offer_before_it_is_given() {
    let mut f = filter("from-host");
    ev(&mut f, 1, Builder::new(DEVICE, 0).uint(OFFER).build());
    assert!(req(&mut f, 1, receive(OFFER), vec![7]).is_err());
}

#[test]
fn host_copy_reaches_the_guest_when_allowed() {
    for policy in ["both", "from-host"] {
        let mut f = filter(policy);
        let (client, server) = split(offer_selection(&mut f, 1, OFFER));
        assert_eq!(client.len(), 3);
        assert_eq!(selection_arg(&client[2].1), OFFER);
        assert!(server.is_empty());
        let (client, server) = split(req(&mut f, 1, receive(OFFER), vec![7]).unwrap());
        assert!(client.is_empty());
        assert_eq!(server, vec![(1, receive(OFFER), vec![7])]);
    }
}

#[test]
fn guest_pastes_its_own_copy_without_the_host() {
    for policy in ["none", "to-host", "from-host", "both"] {
        let mut f = filter(policy);
        req(
            &mut f,
            1,
            Builder::new(DEVICE, 1).uint(SOURCE).uint(1).build(),
            vec![],
        )
        .unwrap();
        let (client, _) = split(offer_selection(&mut f, 1, OFFER));
        assert_eq!(selection_arg(&client[2].1), OFFER);
        // The guest's source is asked directly.
        let (client, server) = split(req(&mut f, 1, receive(OFFER), vec![7]).unwrap());
        assert!(server.is_empty(), "{policy}");
        assert_eq!(
            client,
            vec![(
                1,
                Builder::new(SOURCE, 1).string(b"text/plain").build(),
                vec![7]
            )]
        );
    }
}

#[test]
fn host_paste_of_guest_copy() {
    let send = || Builder::new(SOURCE, 1).string(b"text/plain").build();
    for (policy, passed) in [
        ("none", false),
        ("from-host", false),
        ("to-host", true),
        ("both", true),
    ] {
        let mut f = filter(policy);
        req(
            &mut f,
            1,
            Builder::new(DEVICE, 1).uint(SOURCE).uint(1).build(),
            vec![],
        )
        .unwrap();
        let out = f.handle(1, false, send(), vec![8]).unwrap();
        assert_eq!(!out.is_empty(), passed, "{policy}");
    }
}

#[test]
fn ownership_ends_with_the_source() {
    let mut f = filter("none");
    req(
        &mut f,
        1,
        Builder::new(DEVICE, 1).uint(SOURCE).uint(1).build(),
        vec![],
    )
    .unwrap();
    // A host app copied: the guest's source is cancelled.
    ev(&mut f, 1, Builder::new(SOURCE, 2).build());
    let (client, _) = split(offer_selection(&mut f, 1, OFFER));
    assert_eq!(selection_arg(&client[0].1), 0);

    let mut f = filter("none");
    req(
        &mut f,
        1,
        Builder::new(DEVICE, 1).uint(SOURCE).uint(1).build(),
        vec![],
    )
    .unwrap();
    let (client, _) = split(offer_selection(&mut f, 1, OFFER));
    assert_eq!(selection_arg(&client[2].1), OFFER);
    // The source goes after the offer was given: a paste finds nothing.
    req(&mut f, 1, Builder::new(SOURCE, 1).build(), vec![]).unwrap();
    assert!(req(&mut f, 1, receive(OFFER), vec![7]).unwrap().is_empty());
}

#[test]
fn guest_copy_pastes_across_connections() {
    let mut f = filter("none");
    setup(&mut f, 2);
    req(
        &mut f,
        1,
        Builder::new(DEVICE, 1).uint(SOURCE).uint(1).build(),
        vec![],
    )
    .unwrap();
    let (client, _) = split(offer_selection(&mut f, 2, OFFER));
    assert_eq!(selection_arg(&client[2].1), OFFER);
    let (client, _) = split(req(&mut f, 2, receive(OFFER), vec![7]).unwrap());
    assert_eq!(client[0].0, 1);
    // Once that connection is gone, the selection is the host's.
    f.close(1);
    let (client, _) = split(offer_selection(&mut f, 2, OFFER + 1));
    assert_eq!(selection_arg(&client[0].1), 0);
}

#[test]
fn host_drag_is_kept_from_the_guest() {
    let mut f = filter("none");
    ev(&mut f, 1, Builder::new(DEVICE, 0).uint(OFFER).build());
    let enter = Builder::new(DEVICE, 1)
        .uint(1)
        .uint(10)
        .int(0)
        .int(0)
        .uint(OFFER)
        .build();
    let (client, server) = split(ev(&mut f, 1, enter));
    assert_eq!(
        u32::from_ne_bytes(client[0].1[24..28].try_into().unwrap()),
        0
    );
    assert_eq!(server.len(), 1);
}

#[test]
fn rejects_unknown_objects_and_opcodes() {
    let mut f = filter("both");
    assert!(req(&mut f, 1, Builder::new(77, 0).build(), vec![]).is_err());
    assert!(req(&mut f, 1, Builder::new(DEVICE, 40).build(), vec![]).is_err());
    // Objects in the compositor's range are the compositor's to make.
    assert!(
        req(
            &mut f,
            1,
            Builder::new(MANAGER, 0).uint(OFFER + 5).build(),
            vec![]
        )
        .is_err()
    );
}

#[test]
fn ids_are_reused_after_delete_id() {
    let mut f = filter("none");
    req(&mut f, 1, Builder::new(SOURCE, 1).build(), vec![]).unwrap();
    ev(&mut f, 1, Builder::new(1, 1).uint(SOURCE).build());
    assert!(req(&mut f, 1, Builder::new(SOURCE, 0).build(), vec![]).is_err());
    // wl_compositor.create_surface with the freed id.
    ev(&mut f, 1, global(9, "wl_compositor", 4));
    req(&mut f, 1, bind(9, "wl_compositor", 4, 20), vec![]).unwrap();
    req(&mut f, 1, Builder::new(20, 0).uint(SOURCE).build(), vec![]).unwrap();
    assert_eq!(
        f.fds_needed(
            1,
            true,
            &Builder::new(SOURCE, 1).uint(0).int(0).int(0).build()
        )
        .unwrap(),
        0
    );
}
