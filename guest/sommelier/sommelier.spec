# sommelier for garnish's Fedora guests.
# Don't run rpmbuild on this file: ../build-packages.sh fills in @VERSION@
# and @RELEASE@ from ../versions and makes the source tarball (upstream
# plus patches/), then builds a copy.

# The binary is all we ship; no -debuginfo/-debugsource packages.
%global debug_package %{nil}

Name:           sommelier
Version:        @VERSION@
Release:        @RELEASE@%{?dist}
Summary:        Wayland proxy that shows crosvm guest windows on the host (garnish build)
License:        BSD-3-Clause
URL:            https://chromium.googlesource.com/chromiumos/platform2/+/HEAD/vm_tools/sommelier/
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  meson
BuildRequires:  ninja-build
BuildRequires:  pkgconf-pkg-config
BuildRequires:  python3
BuildRequires:  python3-jinja2
BuildRequires:  systemd-rpm-macros
BuildRequires:  wayland-devel
BuildRequires:  mesa-libgbm-devel
BuildRequires:  libdrm-devel
BuildRequires:  pixman-devel
BuildRequires:  libxcb-devel
BuildRequires:  libxkbcommon-devel

%description
sommelier is ChromiumOS's Wayland proxy. In a garnish guest it forwards
the guest's Wayland clients to the host compositor through crosvm's
cross-domain GPU channel. This build carries garnish's patches and a
systemd user unit that starts it for every user.

%prep
%autosetup

%build
%meson -Dwith_tests=false
%meson_build

%install
%meson_install
garnish/install-extras.sh %{buildroot}

%files
%license garnish/LICENSE
%{_bindir}/sommelier
%{_userunitdir}/sommelier.service
%{_userunitdir}/default.target.wants/sommelier.service
