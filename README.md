# legacy-plugins

Runs the classic assistant plugins (Razor, ClassicAssist) on the new ClassicUO client.

Each plugin works as it does on the old client:

- sees every packet the client sends and receives, and can let it through, block it or rewrite it
- can send its own packets to the server or to the client
- can walk, cast spells, use abilities and set the window title
- gets hotkeys and mouse buttons; a key a plugin uses doesn't also trigger the client's own hotkey
- is told the player's position, and when the client connects, disconnects, gains or loses focus, or closes

## Setup

List the plugins in the mod's own `Data/Mods/legacy-plugins/storage.json`, relative to `Data/Plugins` next to the exe:

```json
{
  "plugins": [ "ClassicAssist/ClassicAssist.dll" ]
}
```

A `plugins` list from an older `settings.json` is copied there once, the first time the client starts without one.

Windows desktop only. Plugins load only when the client is started through `ClassicUO.exe`.

## Build

Needs Rust with the `wasm32-wasip2` target. `make build` writes the mod to `dist/legacy-plugins/`; copy that folder into the client's `Data/Mods/`.

## Known gaps

- Autowalk state is estimated (on when a plugin starts a walk, off when it arrives or cancels).
- Connect and focus events still reach plugins while the mod is disabled in the mod list.
