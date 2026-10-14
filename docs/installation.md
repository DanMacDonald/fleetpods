# Install FleetPods

FleetPods installs for the current user. Run these steps from an Omarchy desktop session.

## Requirements

- Omarchy with the shell plugin system and `omarchy plugin` commands.
- Bluetooth with support for scanning and advertising over Bluetooth Low Energy.
- PipeWire, WirePlumber 0.5, BlueZ, and an unlocked login keyring providing Secret Service, such as GNOME Keyring.
- Git, Rust and Cargo, a C compiler, and pkg-config for building from source.

Install the native build dependencies and Qt modules:

```sh
omarchy pkg add base-devel git pkgconf ffmpeg pipewire libpulse dbus qt6-5compat qt6-svg
```

If you do not already have Rust and Cargo, install them:

```sh
omarchy pkg add rust
```

An existing Rust toolchain is fine. Make sure `cargo` and `~/.local/bin` are on your PATH. FleetPods does not declare a minimum Rust version; use a current stable toolchain.

## Build and install

```sh
git clone https://github.com/DanMacDonald/fleetpods.git
cd fleetpods
./setup
```

The installer builds the daemon and CLI, enables the user service, adds the plugin to the bar, and installs microphone support. It restarts WirePlumber when the microphone hooks change, which briefly interrupts audio.

Confirm the service is running:

```sh
systemctl --user is-active fleetpods.service
fleetpods status
```

The service should report `active`. Being unpaired on a fresh install is expected. Open the FleetPods icon for pairing guidance.

## First connection

Pair the AirPods separately on each Omarchy machine through its Bluetooth panel. For AirPods Pro 3, leave both buds in the open case and double-tap the front until the light flashes white. Select the AirPods in the Bluetooth panel and leave the case open until pairing and connecting finish.

FleetPods uses nearby case reports to show instructions for the detected model. Follow those instructions for other models; their support is described in [Supported hardware](support.md).

To share between Omarchy machines, install FleetPods on both and keep them nearby. Select **Add to fleet** to invite a nearby computer, or **Join fleet** to join one that already has a fleet. Enter the displayed code on the invited computer. Joining is complete when both computers confirm success. Bluetooth pairing and fleet joining are separate steps.

If you already belong to another fleet, joining asks whether to leave it on this computer. Declining an incoming invitation ends that invitation.

For calls or recording, choose your AirPods as the input device in the application's audio settings. FleetPods supplies the high-resolution input for AirPods Pro 3 while keeping the stereo playback profile.

## Upgrade

From the checkout, run:

```sh
git pull --ff-only
./setup
```

Saved pairing, fleet membership, and preferences are kept. If the bar still shows an old view after an upgrade, reload its cached components:

```sh
omarchy restart shell
```

To install binaries you have already built for the target machine, use `./setup --bin-dir /path/to/binaries`. That directory must contain both `fleetpods` and `fleetpodsd`; the plugin and microphone files still come from the checkout.

## Remove FleetPods

These steps remove the service, plugin, binaries, and microphone hooks:

```sh
systemctl --user disable --now fleetpods.service
omarchy plugin remove io.github.danmacdonald.fleetpods --yes

rm -f "$HOME/.local/bin/fleetpods" "$HOME/.local/bin/fleetpodsd" \
  "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/fleetpods.service" \
  "${XDG_CONFIG_HOME:-$HOME/.config}/wireplumber/wireplumber.conf.d/90-fleetpods-microphone.conf" \
  "${XDG_DATA_HOME:-$HOME/.local/share}/wireplumber/scripts/fleetpods-microphone.lua"

systemctl --user daemon-reload
systemctl --user restart wireplumber
```

Omarchy backs up the plugin folder installed by `./setup`. Saved FleetPods data, keyring records, and Bluetooth pairing remain available for a later reinstall. The last command briefly interrupts audio while removing microphone support.

To leave this computer's fleet before uninstalling, run `fleetpods fleet reset` while its daemon is still running. To remove this computer from another member's saved list, run `fleetpods fleet remove <Bluetooth address>` on that other computer. Removing a member does not rotate the shared fleet key.

For connection problems, see [Support and troubleshooting](support.md).
