# Makepad 2.0: Reference for Wallet Native

As of 2026-10-29. Collected through Moirai from the official repo (`makepad/makepad`, `dev` branch), makepad.nl, and the `ZhangHanDong/makepad-skills` / `makepad-2.0-book` notes (dev snapshot, March–April 2026). Companion to [wallet-native-brief.md](wallet-native-brief.md) §8.

Makepad moves fast and the dev branch is the source of truth. Where the community notes and the official examples disagree, the official example wins (flagged below).

---

## 1. What Makepad 2.0 is

- A Rust UI runtime for desktop, Android, iOS/tvOS, web (wasm) and Quest, GPU-rendered (2D, SDF shaders, simple 3D).
- **Splash** is the UI language: a runtime-evaluated script, written inside `script_mod! { … }` in your Rust files. It replaced the 1.x compile-time `live_design!` macro, so **any 1.x tutorial or blog post (e.g. older Robius/Edward Tan guides) uses outdated syntax**.
- Splash edits apply live (`--hot`) without recompiling; Rust changes still need a rebuild.
- Split of responsibilities: Splash = layout, styling, templates, small inline interactions. Rust = app lifecycle, events, business logic, I/O. For Wallet that means `wallet-core`/`wallet-store` stay pure Rust and the app crate only bridges them to Splash state.

**Showcase:** makepad.nl has web demos, including **Finance** ("personal finance on a generated two-year household ledger — overview, transactions, budget, reports"). Open it before designing screens. There are also Sheets, Files, DJ/VJ, Maps and Score demos.

---

## 2. Setup and build commands

### Clone and run (desktop)

```bash
git clone https://github.com/makepad/makepad.git
cd makepad
cargo run -p makepad-example-counter --release
cargo run -p makepad-example-splash --release
cargo run -p makepad-director --release     # Studio: run, inspect and iterate on examples
```

Rust stable is enough for native builds. Run your own app with hot reload:

```bash
cargo run -p wallet-app -- --hot
```

### Android

Plug in a phone with developer mode + USB debugging on. The toolchain install is local to the makepad checkout and does not touch your PATH.

```bash
# from the makepad checkout
cargo run -p cargo-makepad --release -- android toolchain-install               # arm64 only
cargo run -p cargo-makepad --release -- android --target=all toolchain-install  # every ABI

cargo run -p cargo-makepad --release -- android run -p makepad-example-ironfish
cargo run -p cargo-makepad --release -- android --package-name=com.escillex.wallet --app-label="Wallet" run -p wallet-app
```

Or install the tool once and use the short form:

```bash
cargo install --path=./tools/cargo_makepad
cargo makepad android --abi=all install-toolchain
cargo makepad android run -p wallet-app
```

> The README and `tools/android/README.md` use slightly different flag spellings (`--target=all toolchain-install` vs `--abi=all install-toolchain`). The `cargo run -p cargo-makepad -- android …` form is the one in the Android README; use it if the short one errors.

After the first install, grant the app any runtime permissions it asks for in Android settings.

### Web (optional)

```bash
cargo makepad wasm install-toolchain
cargo makepad wasm run -p wallet-app --release        # serves http://127.0.0.1:8010
cargo makepad wasm build -p wallet-app --profile=small --strip --split --brotli
```

### Linux / WSL deps

Only needed if building in the Arch WSL instead of Windows. The README lists Ubuntu packages (X11, Wayland, Mesa/GL/Vulkan, ALSA/Pulse, GStreamer, clang, pkg-config, libssl). Translate them to pacman if you go that way.

---

## 3. App skeleton

Official `examples/counter` (dev branch), reformatted:

```toml
# Cargo.toml
[package]
name = "makepad-example-counter"
version = "1.0.0"
edition = "2021"

[features]
default = ["tweaker"]
# Design overlay (Shift+F10 with dev overlays on). Release builds use --no-default-features.
tweaker = ["makepad-widgets/tweaker"]

[dependencies]
makepad-widgets = { path = "../../widgets", version = "2.0.0", default-features = false }

[dev-dependencies]
makepad-test = { path = "../../libs/makepad_test", version = "0.1.0" }
```

For Wallet, point `makepad-widgets` at a git dependency or a sibling checkout instead of `../../widgets`.

```rust
// src/main.rs
pub use makepad_widgets;
use makepad_widgets::*;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    let state = mod.state

    startup() do #(App::script_component(vm)){
        ui: Root{
            on_startup: ||{
                // render isn't called automatically yet
                ui.main_view.render()
            }
            main_window := Window{
                window.inner_size: vec2(420, 220)
                body +: {
                    main_view := View{
                        width: Fill height: Fill
                        flow: Down spacing: 12 align: Center
                        on_render: ||{
                            app_icon := AppIcon{name: "counter" width: 48 height: 48}
                            counter_label := Label{
                                text: "Count: " + state.counter
                                draw_text.text_style.font_size: 24
                            }
                        }
                    }
                    increment_button := Button{ text: "Increment" }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live] ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(increment_button)).clicked(actions) {
            script_eval!(cx, {
                mod.state.counter += 1
                ui.main_view.render()
            });
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        // App data survives Splash/style reloads; the UI is re-evaluated against this same state.
        if !vm.is_reload() {
            script_eval!(vm, { mod.state = {counter: 0} true });
        }
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
```

**Key pieces**

| Piece | Role |
| --- | --- |
| `app_main!(App)` | Entry point, module level |
| `script_mod! { … }` | The Splash source: imports, state, functions, templates, then `startup() do …{ ui: Root{…} }` |
| `#[derive(Script, ScriptHook)]` | Replaces 1.x `Live`/`LiveHook`. `#[live]` fields are set from Splash; `#[rust]` fields are Rust-only (timers, DB handles) |
| `AppMain::script_mod` | Registers widgets, seeds `mod.state` once (`!vm.is_reload()` keeps data across hot reloads), then loads your module |
| `handle_event` | Must call **both** `self.match_event(…)` and `self.ui.handle_event(…)` |

> Older notes (March 2026) show `impl App { fn run(vm) -> Self { …; App::from_script_mod(vm, self::script_mod) } }` instead of `AppMain::script_mod`. The current counter uses the form above.

Theme selection (from the notes; check against the dev branch): register `theme_mod(vm)`, then `script_eval!(vm, { mod.theme = mod.themes.light })`, then `widgets_mod(vm)`, then your own modules. Order matters: theme, then base widgets, then custom widgets, then the app.

---

## 4. Rust ↔ Splash bridge

**Rust → Splash**

```rust
// run Splash code: update state, re-render a view
let label = format!("{}", balance);
script_eval!(cx, {
    mod.state.balance = #(label)
    ui.main_view.render()
});

// patch properties on a specific widget
script_apply_eval!(cx, self.ui, {
    title.text: "New title"
    card.draw_bg +: { color: #(vec4(0.1, 0.1, 0.1, 1.0)) }
});
```

`#(expr)` injects a Rust value into Splash.

**Reading widgets from Rust** (`self.ui.<type>(cx, ids!(path.to.widget))`):

| Widget | Methods |
| --- | --- |
| `button` | `.clicked(actions)`, `.pressed`, `.long_pressed`, `.released`, `.clicked_modifiers` |
| `text_input` | `.changed(actions) -> Option<String>`, `.returned(actions)`, `.escaped(actions)`, `.text()`, `.set_text(cx, …)` |
| `label` | `.set_text(cx, …)` |
| `check_box` | `.changed(actions) -> Option<bool>` |
| `drop_down` | `.selected(actions)`, `.changed_label(actions)`, `.set_selected_item(cx, i)`, `.set_labels(cx, vec)` |
| `slider` | `.slided(actions)`, `.end_slide(actions)`, `.value()`, `.set_value(cx, v)` |
| any | `.widget(cx, ids!(…)).redraw(cx)`, `.set_visible(cx, bool)` |

**`MatchEvent` hooks** worth knowing: `handle_startup`, `handle_pause` / `handle_resume` / `handle_background` (save or sync on Android backgrounding), `handle_back_pressed -> bool` (Android back button), `handle_key_down`, `handle_timer`, `handle_next_frame`, `handle_http_response` / `handle_http_request_error`, `handle_signal`.

**Timers**

```rust
#[rust] sync_timer: Timer,

fn handle_startup(&mut self, cx: &mut Cx) { self.sync_timer = cx.start_interval(60.0); }
fn handle_timer(&mut self, cx: &mut Cx, e: &TimerEvent) {
    if self.sync_timer.is_timer(e).is_some() { /* push/pull */ }
}
// cx.start_timeout(secs) for one-shot, cx.stop_timer(t)
```

**HTTP from Rust** (usable for `wallet-sync`, or keep `wallet-sync` on its own client):

```rust
let mut req = HttpRequest::new(url, HttpMethod::Post);
req.set_header("Authorization", &format!("Bearer {token}"));
req.set_body(body_bytes);
cx.http_request(live_id!(sync_push), req);
// result arrives in handle_http_response(cx, request_id, &HttpResponse)
```

---

## 5. Splash language essentials

### Rules that bite

- `let` / `fn` declarations go **first**, then the root widget.
- **Every container needs `height: Fit`** unless its parent has a fixed or Fill height. The default is `Fill`, and Fill inside Fit resolves to **0 px** (invisible UI). This is the most common bug.
- `name := Widget{…}` makes a child addressable (`ui.name`, overridable); `name: Widget{…}` or no name is static. **A `:=` id is only a field of its direct parent**: name every wrapper on the path (`Card{inner.b.text: "x"}`), or the override silently fails.
- Text defaults to **white**. On light backgrounds set `draw_text.color` on every text element.
- Containers with `show_bg: true` and text children need **`new_batch: true`**, or the text draws behind the background.
- Hex colours with `e` next to digits need `#x`: `#x1e6bff`, `#x0b1020`. (Relevant: several Wallet swatches contain `e`.)
- `draw_bg.border_radius` takes a float (`16.0`), not an `Inset`.
- Every `TextInput` needs a fixed numeric height (e.g. `34`).
- Numbers to strings: `"" + n` or `n + ""`. Don't use `as int` (gives NaN). String `+` concatenates (`"5" + "3" == "53"`).
- `for` loops in the tree render **once** at build time. For changing data use `on_render` + `ui.<view>.render()`.
- `on_render` doesn't run by itself; call `.render()` (e.g. in `on_startup`).
- Items in arrays / object bodies can be separated by spaces, newlines or commas. To extend an object use `base += {field: v}` or a named prototype, not `{…}{…}`.

### Layout ("turtle", not flexbox)

| Property | Values |
| --- | --- |
| `width` / `height` | `Fill` (default), `Fit`, `200`, `Fill{min: 100 max: 500}`, `Fit{max: Abs(300)}` |
| `flow` | `Right` (default), `Down`, `Overlay` (stacked), `Flow.Right{wrap: true}` |
| `spacing` | gap between children |
| `padding` / `margin` | `10` or `Inset{top: 8 bottom: 8 left: 12 right: 12}` |
| `align` | `Align{x: 0.5 y: 0.5}` (0 = start, 1 = end), or `Center` |
| `Filler{}` | pushes siblings apart (between Fit siblings) |

`flow: Overlay` is the tool for the big number with a hidden input on top, or a badge over the dial.

### State, functions, templates

```splash
let items = [{text: "A" done: false} {text: "B" done: false}]

fn toggle(i){
    items[i] += {done: !items[i].done}
    ui.list.render()
}

let Row = RoundedView{
    width: Fill height: Fit flow: Right spacing: 10
    padding: Inset{top: 8 bottom: 8 left: 12 right: 12}
    new_batch: true
    draw_bg.color: #x2a2a3a
    draw_bg.border_radius: 8.0
    label := Label{text: "" width: Fill}
    del := ButtonFlatter{text: "✕" width: 40 height: 28}
}

list := ScrollYView{
    width: Fill height: Fill new_batch: true
    on_render: ||{
        for i, it in items {
            Row{label.text: it.text  del.on_click: || toggle(i)}
        }
    }
}
```

Arrays support `.push()`, `.remove(i)`, `.len()`, `.retain(|x| …)`, `.clear()`; read `a[i].field`, update `a[i] += {field: v}`. Top-level `let` values persist across clicks.

### Inline events

| Handler | On | Notes |
| --- | --- | --- |
| `on_click: ||{…}` | buttons; `|checked|` on CheckBox | |
| `on_return: |text| …` | TextInput | Enter key |
| `on_render: ||{…}` | views | rebuilds the view's content |
| `on_startup: ||{…}` | `Root` | once at launch |

Inside handlers: `ui.x.text()`, `ui.x.set_text(…)`, `ui.x.render()`, `ui.btn.on_click()`.

> The `splash.md` notes for AI-chat mini-apps say `ui` is **not** visible in a separately declared `fn` there, while the todo example calls `ui.todo_list.render()` from an `fn`. In a full app, `fn` + `ui` works per the examples. If you see "variable ui not found", move the call inline.

Debug output: `std.println("x = " + x)` shows in the terminal and in Studio's Log View. There are no breakpoints in Splash.

---

## 6. Widget catalog

**Containers:** `View`, `SolidView`, `RoundedView`, `RoundedAllView`, `GradientXView`, `GradientYView`, `ScrollXView`, `ScrollYView`, `ScrollXYView`.
**Text:** `Label`, `H1`–`H4`, `P`, `TextInput` (`empty_text`, `password`, `read_only`, `numeric_only`), `Markdown`, `Html`, `LinkLabel`.
**Buttons:** `Button`, `ButtonFlat`, `ButtonFlatter`.
**Toggles:** `CheckBox`, `Toggle`, `RadioButton` (+ `Flat` variants), `ToggleFlat`.
**Input:** `Slider`, `DropDown` (`labels: [...]`).
**Media:** `Image` (`fit: ImageFit.CropToFill`), `Svg`, `Icon`, `Vector` (inline paths), `LoadingSpinner`.
**Layout helpers:** `Hr`, `Vr`, `Filler`, `Splitter`, `FoldHeader`.
**Lists:** `PortalList` (virtualised, use for History), `FlatList` (small lists).
**Navigation:** `Modal` (`.open(cx)` / `.close(cx)`), `Tooltip`, `PopupNotification`, `SlidePanel`, `ExpandablePanel`, `PageFlip` (`active_page`), `StackNavigation` (push/pop).
**Dock:** `Dock`, `DockSplitter`, `DockTabs`, `DockTab` (desktop IDE-style; not needed).
**Glass kit:** `glass.Card`, `glass.Panel`, `glass.GlassButton`, `glass.TextInput`, `glass.H1`… (Apple-style "liquid glass"). Not Wallet's look.

**Known not to work / missing** (per `splash.md`): `Stack`, `Divider` (use `Hr`), `ProgressBar` (use a read-only `Slider` or a drawn bar), `IconButton`, `ToggleButton`, `ListView`, `Grid`, `ColorPicker`. Core `TabBar`/`Tab` render at zero size; build tabs from a row of `ButtonFlat`.

**Widget checked state** (`CheckBox`, `RadioButton`, `ToggleFlat`) doesn't reliably persist into Splash variables; track the boolean yourself.

---

## 7. Styling, theming, animation

| Property | Example |
| --- | --- |
| `draw_bg.color` | `#000000`, `#x1e6bff` |
| `draw_bg.border_radius` | `10.0` |
| `draw_text.color` | `#fff` |
| `draw_text.text_style.font_size` | `24` |
| Label shorthand | `Label{text: "X" color: #fff font_size: 16}` |
| `theme.*` | `theme.space_1`, `theme.color_*` (prefer over hardcoding for app-wide palette) |

**Per-wallet theming:** keep `bg` / `ink` / `accent` in `mod.state` and reference them from templates, or patch live with `script_apply_eval!`. The 350 ms fade can be an animator on the root view or a `NextFrame`-driven lerp in Rust.

**Animator** (drives shader `instance` values over time):

```splash
View{
    animator: Animator{
        hover: {
            default: @off
            off: AnimatorState{ from: {all: Forward {duration: 0.15}} apply: {draw_bg: {hover: 0.0}} }
            on:  AnimatorState{ from: {all: Forward {duration: 0.15}} apply: {draw_bg: {hover: 1.0}} }
        }
    }
    Label{text: "Animated via parent"}
}
```

Play types: `Forward {duration}`, `Snap`, `Loop {duration}`, `ReverseLoop`, `Bounce`. Groups (`hover`, `focus`, `active`, `disabled`, `time`) run independently. Rust side: `self.animator_play(cx, ids!(hover.on))`.

Animator works on `View`/`SolidView`/`RoundedView`/scroll views/buttons/toggles/`TextInput`. It is **silently ignored** on `Label`, `H1`–`H4`, `Image`, `Icon`, `Slider`, `DropDown`, `Hr`; wrap those in a `View`.

**Fonts:** Wallet's look needs Barlow Condensed 900 italic, Space Grotesk and JetBrains Mono as bundled resources (`crate_resource(...)`). Check the `text_style` / font-family docs in the widgets crate when wiring this up.

---

## 8. Custom widgets and raw input

The wallet dial, drag-to-adjust entry and the calendar swipe all need custom widgets with raw pointer handling:

```rust
impl Widget for WalletDial {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(fd) => { cx.set_key_focus(self.draw_bg.area()); /* fd.abs, fd.rel */ }
            Hit::FingerMove(fm) => { /* fm.abs: drag delta, velocity for fling */ }
            Hit::FingerUp(fu)   => { if fu.is_over { /* tap */ } /* start momentum */ }
            Hit::FingerScroll(fs) => { /* mouse wheel / trackpad */ }
            Hit::KeyDown(ke) => match ke.key_code {
                KeyCode::ArrowLeft | KeyCode::ArrowRight => { /* step */ }
                _ => {}
            },
            _ => {}
        }
    }
}
```

Other `Hit` variants: `FingerLongPress`, `FingerHoverIn/Over/Out`, `KeyUp`, `TextInput`, `TextCopy`/`TextCut`, `ImeAction`, `KeyFocus`/`KeyFocusLost`. Emit results to the app with `cx.widget_action(self.widget_uid(), MyAction::Changed(v))`. Drive momentum from `NextFrame` (`cx.new_next_frame()`), as `detent.svelte.ts` does with `requestAnimationFrame`.

Custom drawing goes through `draw_bg` shaders (`pixel fn` with `Sdf2d`); see the book's rendering chapters (ch18–21) and the `makepad-2.0-shaders` notes. Good for day boxes, bar charts and the calendar grid cells.

---

## 9. Mapping to Wallet

| Wallet piece | Makepad approach |
| --- | --- |
| Setup (wallet rows, + Wallet) | `on_render` list from `mod.state.setup_rows`; `TextInput` with fixed height; validation in Rust (`wallet-core`) |
| Mode switch | Two `ButtonFlat`s or a small custom widget; state in `mod.state.mode` |
| Wallet dial | **Custom widget** (Hit events + `NextFrame` momentum + detents). Prototype first |
| Rolling number | Custom widget or animator-driven offset per digit; plain `Label` first |
| Amount entry | Custom widget around a focused `TextInput` (keyboard on phone + PC) with drag handling via `Hit::FingerMove` / `FingerScroll`. Prototype first |
| Day boxes | Row of small `RoundedView`s from `on_render`, or one custom-drawn widget |
| Calendar grid | `Flow.Right{wrap: true}` of fixed-width cells, or a custom widget (also handles sideways swipe) |
| Bar charts | Custom `draw_bg` shader or a row of fixed-height views |
| Manage tabs | Row of `ButtonFlat` + `PageFlip` (core `TabBar` is broken) |
| History | `PortalList` (virtualised) |
| Two-tap delete | Track the "armed" id in state; re-render |
| Theme fade | Animator on root, or lerp colours on `NextFrame` and `script_apply_eval!` |
| Haptics, `.escb` file picker, notifications | Not in the widget set; needs Android glue (JNI) or a platform crate. Check what `makepad-platform` exposes |
| Android back | `MatchEvent::handle_back_pressed` |
| Save / sync on background | `handle_pause` / `handle_background` |

---

## 10. Further reading

- Repo + README: https://github.com/makepad/makepad (`dev` branch)
- Splash DSL guide: `splash.md` in the repo root
- Android build: `tools/android/README.md`
- Examples worth reading: `examples/counter`, `examples/todo`, `examples/text_input`, `examples/charts`, `examples/portallist_hit`, `examples/uizoo` (widget gallery), `examples/glass`
- Showcase + Finance demo: https://makepad.nl
- *Makepad 2.0: From Zero to Architecture* (Chinese, English planned): https://github.com/ZhangHanDong/makepad-2.0-book · read online https://zhanghandong.github.io/makepad-2.0-book/
- Claude Code skills for Makepad 2.0 (app-structure, dsl, layout, widgets, events, animation, shaders, theme, troubleshooting, performance, migration): https://github.com/ZhangHanDong/makepad-skills. Worth installing before writing the app
- cargo-makepad CLI overview: https://deepwiki.com/makepad/makepad/5.1-cargo-makepad-cli
- Discord: https://discord.gg/adJcZVjXrV
