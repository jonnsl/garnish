# garnish-guest for garnish's Fedora guests: carte-agent, maitre-ready and
# the Wayland environment.
# Don't run rpmbuild on this file: ../build-packages.sh fills in @VERSION@
# and @RELEASE@ from ../versions, makes the source tarball and fetches the
# crates, then builds a copy.

%global debug_package %{nil}

Name:           garnish-guest
Version:        @VERSION@
Release:        @RELEASE@%{?dist}
Summary:        Guest side of garnish: carte-agent, boot notification for maitred, Wayland environment
License:        GPL-3.0-or-later
URL:            https://github.com/jonnsl/garnish
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  systemd-rpm-macros
Requires:       sommelier
Requires:       systemd

%description
The parts of garnish that run in a guest: carte-agent, which shows the
guest's applications in the host's application menu, maitre-ready.service,
which tells maitred when the guest has booted, and the environment that
makes applications use Wayland.

%prep
%autosetup

%build
cargo build --frozen --release --manifest-path carte/Cargo.toml \
    --target-dir target --bin carte-agent

%install
./install.sh %{buildroot} target/release/carte-agent

%files
%license LICENSE
%{_bindir}/carte-agent
%{_userunitdir}/carte-agent.service
%{_userunitdir}/default.target.wants/carte-agent.service
%{_unitdir}/maitre-ready.service
%{_unitdir}/multi-user.target.wants/maitre-ready.service
%{_prefix}/lib/environment.d/50-garnish-wayland.conf
%config(noreplace) %{_sysconfdir}/profile.d/garnish-wayland.sh
