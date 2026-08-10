# Plano: `symbian-decl-ui` — Declarative UI SDK com Builder Pattern

## Resumo

Criar um crate `symbian-decl-ui` que oferece uma API declarativa estilo builder pattern (similar a Jetpack Compose sem compiler plugin, ou Flutter com builders) para construir interfaces no Symbian. O crate roda sobre o `symbian-gfx` e `symbian-ui` existentes, sem substituí-los.

## Decisões de Design

| Decisão | Escolha |
|---------|---------|
| Sintaxe | Builder pattern (`.child().child()`) — Rust puro, sem proc-macro |
| Children de containers | `Vec<Box<dyn Widget>>` — alocado 1x na construção, não por frame |
| Cache de medida | `content_hash() -> u64` — recalcula measure só quando propriedades mudam |
| Cache de layout | Rects armazenados no `UiCache` por slot após `layout_tree()` |
| Estado local | `SlotTable` — `use_state()`, `begin_group(key)`. Posição no source = identidade |
| Estado global | MVU: `Model` struct + `update(Model, Msg) -> Cmd` (Elm-style) |
| Efeitos colaterais | `Cmd` enum (`None`, `Exit`, `SetTimer`, `Connect`, `Send`, `PushScreen`, `PopScreen`) |
| Sem suporte a C++ | Somente Rust |
| Sem hot reload | Não necessário no simulador por enquanto |
| Integração Telegram | Feature flag `declarative` no crate existente; rewrite do Telegram na Fase 6 |
| Virtual DOM | **Não**. Draw imediato como o SDK atual. 320×240 = O(árvore) > O(pixels) |
| Trait objects | Sim (`Box<dyn Widget>`), mas sem alocação por frame |
| Three-tree (Flutter) | **Não**. Uma árvore de Widgets com cache de layout é suficiente |
| Gesture/touch | **Não**. Sem touchscreen. D-pad + teclado = dispatch direto |

## Crate Graph

```
symbian-gfx         (existe)    Canvas, Color, Font, Rect, Size, Point, Edges
    ↓
symbian-ui          (existe)    Theme, Palette, Metrics, Surface, Space, Key, KeyEvent,
                                Handled, App trait, chrome, list, edit, paint, icon, tokens
    ↓
symbian-decl-ui     (NOVO)      Widget trait, Constraints, Length, UiCache, SlotTable,
                                Cmd, Msg, + widgets: Screen, Row, Column, Text, ScrollList...
    ↓
apps/telegram       (existe)    Continua usando symbian-ui diretamente, mas ganha a
                                opção de implementar telas com symbian-decl-ui
```

### Novo arquivo: `crates/symbian-decl-ui/Cargo.toml`

```toml
[package]
name = "symbian-decl-ui"
version = "0.1.0"
edition = "2021"

[dependencies]
symbian-gfx = { path = "../symbian-gfx", default-features = false }
symbian-ui = { path = "../symbian-ui", default-features = false }

[features]
default = []
```

O crate **não** depende de `symbian-sys`, `symbian`, `symbian-app`, nem da std. É `no_std` + `alloc` puro, como os crates existentes.

---

## Camada 1: Tipos Fundamentais

### `Length` — dimensão declarativa

```rust
// crates/symbian-decl-ui/src/length.rs

pub enum Length {
    Exact(i32),        // pixels exatos
    Fill(i32),         // ocupa espaço restante do pai (peso proporcional)
    WrapContent,       // tamanho mínimo para caber o conteúdo
}
```

### `Constraints` — o que o pai oferece ao filho

```rust
// crates/symbian-decl-ui/src/constraints.rs

pub struct Constraints {
    pub min_w: i32,
    pub max_w: i32,
    pub min_h: i32,
    pub max_h: i32,
}

impl Constraints {
    pub const fn tight(w: i32, h: i32) -> Self {
        Self { min_w: w, max_w: w, min_h: h, max_h: h }
    }

    pub const fn loose(max_w: i32, max_h: i32) -> Self {
        Self { min_w: 0, max_w, min_h: 0, max_h }
    }

    pub fn constrain(&self, size: Size) -> Size {
        Size::new(
            size.w.clamp(self.min_w, self.max_w),
            size.h.clamp(self.min_h, self.max_h),
        )
    }
}
```

### `FontRole` — referência a fontes do tema

```rust
// crates/symbian-decl-ui/src/theme.rs

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum FontRole {
    Body,
    Strong,
    Small,
    Title,
}

impl Fonts<'_> {
    fn resolve(&self, role: FontRole) -> &dyn Font {
        match role {
            FontRole::Body => self.body,
            FontRole::Strong => self.strong,
            FontRole::Small => self.small,
            FontRole::Title => self.title,
        }
    }
}
```

---

## Camada 2: `Widget` trait + Sistema de Cache

### `Widget` trait — o contrato central

```rust
// crates/symbian-decl-ui/src/widget.rs

pub type WidgetHash = u64;

pub trait Widget {
    /// Hash de todas as propriedades que afetam o tamanho intrínseco.
    /// Retorno padrão é 0 (sempre recalcular measure).
    /// Text deve hashear o conteúdo; Row deve hashear número de filhos + gap + padding.
    fn content_hash(&self) -> WidgetHash { 0 }

    /// Calcula o tamanho intrínseco. Só chamado quando content_hash muda.
    fn measure(&self, constraints: Constraints, theme: &Theme) -> Size;

    /// Desenha no rect alocado. Sempre chamado (nunca cacheado).
    fn draw(&self, c: &mut Canvas, rect: Rect, theme: &Theme);

    /// Evento de tecla. Default: ignora.
    fn handle_key(&self, _ev: KeyEvent, _rect: Rect) -> Handled { Handled::Ignored }

    /// Filhos diretos (para layout de containers). Default: vazio (leaf widget).
    fn children(&self) -> &[Box<dyn Widget>] { &[] }

    /// Peso flex no container pai. 0 = fixo, >0 = proporcional.
    fn flex_weight(&self) -> i32 { 0 }

    /// Gap entre filhos (para Row/Column). Default: 0.
    fn gap(&self) -> i32 { 0 }
}
```

### `UiCache` — cache de medida e layout

```rust
// crates/symbian-decl-ui/src/cache.rs

pub struct UiCache {
    entries: Vec<CacheEntry>,
    cursor: usize,
    generation: u32,
}

struct CacheEntry {
    content_hash: WidgetHash,
    size: Option<Size>,
    rect: Rect,
    gen: u32,
}

impl UiCache {
    pub fn new() -> Self { Self::with_capacity(64) }

    pub fn with_capacity(cap: usize) -> Self {
        Self { entries: Vec::with_capacity(cap), cursor: 0, generation: 1 }
    }

    /// Reset entre frames. Mantém as entries mas avança a geração.
    pub fn begin_frame(&mut self) {
        self.cursor = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Obtém size cacheado ou chama measure().
    pub fn measure_or_compute(
        &mut self,
        widget: &dyn Widget,
        constraints: Constraints,
        theme: &Theme,
    ) -> Size {
        let hash = widget.content_hash();
        if self.cursor < self.entries.len() {
            let entry = &mut self.entries[self.cursor];
            if entry.content_hash == hash && entry.size.is_some() {
                entry.gen = self.generation;
                let size = entry.size.unwrap();
                self.cursor += 1;
                return size;
            }
        }
        let size = widget.measure(constraints, theme);
        self.set_entry(hash, size);
        size
    }

    fn set_entry(&mut self, hash: WidgetHash, size: Size) { /* ... */ }
    pub fn set_rect(&mut self, slot: usize, rect: Rect) { /* ... */ }
    pub fn rect(&self, slot: usize) -> Option<Rect> { /* ... */ }
}
```

### Fluxo por frame

```
1. cache.begin_frame()        → cursor=0, generation++
2. root.measure(cache)        → só recalcula widgets cujo content_hash mudou
3. layout(root, cache)        → calcula rects, armazena no cache
4. root.draw(canvas, cache)   → lê rects do cache, desenha
```

### Exemplo de cache com tela de chat

```
Screen           slot 0  hash=0xAAAA  size=(320,240)
├─ TitleBar      slot 1  hash=0xBBBB  size=(320,18)    ← cache hit se title não mudou
├─ ScrollList    slot 2  hash=0xCCCC  size=(320,205)
│  ├─ Row[0]     slot 3  hash=0xD000  size=(320,38)    ← cache hit se chat não mudou
│  ├─ Row[1]     slot 4  hash=0xD001  size=(320,38)
│  └─ Row[2]     slot 5  hash=0xD002  size=(320,38)
└─ SoftkeyBar    slot 6  hash=0xEEEE  size=(320,17)
```

Frame 1: 7 chamadas a `measure()`. Frame 2 (sem mudanças): **0 chamadas** — todos são cache hits. Frame N (nova mensagem): só Row[N] recalcula.

### Exemplo de `content_hash()` por widget

```rust
impl Widget for Text {
    fn content_hash(&self) -> WidgetHash {
        let mut h: u64 = 0x517cc1b727220a95;
        h ^= self.text.len() as u64;
        for (i, &b) in self.text.as_bytes().iter().take(8).enumerate() {
            h = h.wrapping_mul(0x100000001b3);
            h ^= (b as u64) << (i * 8);
        }
        h ^= (self.font as u8) as u64;
        h ^= (self.max_lines as u64) << 8;
        h
    }
}

impl Widget for Row {
    fn content_hash(&self) -> WidgetHash {
        let mut h: u64 = 0x517cc1b727220a95;
        h ^= self.children.len() as u64;
        h ^= (self.gap as u64) << 16;
        h ^= (self.padding.left as u64) << 24;
        h ^= (self.padding.top as u64) << 32;
        h
    }
}
```

---

## Camada 3: Layout Engine

### Pipeline: measure → layout → draw

```rust
// crates/symbian-decl-ui/src/layout.rs

/// Mede toda a árvore usando cache.
pub fn measure_tree(
    widget: &dyn Widget,
    constraints: Constraints,
    theme: &Theme,
    cache: &mut UiCache,
) -> Size {
    let size = cache.measure_or_compute(widget, constraints, theme);
    for child in widget.children() {
        measure_tree(child.as_ref(), constraints, theme, cache);
    }
    size
}

/// Calcula rects para todos os widgets.
/// Assume que measure_tree() já foi chamado.
pub fn layout_tree(
    widget: &dyn Widget,
    constraints: Constraints,
    theme: &Theme,
    cache: &mut UiCache,
    parent_rect: Rect,
    slot_start: usize,
) {
    // Obtém size (já cacheado)
    let size = cache.measure_or_compute(widget, constraints, theme);
    // Aloca rect do widget
    let rect = Rect::from_xywh(parent_rect.x0, parent_rect.y0,
        size.w.min(parent_rect.width()), size.h.min(parent_rect.height()));
    cache.set_rect(slot_start, rect);

    let mut child_slot = slot_start + 1;
    let children = widget.children();
    if !children.is_empty() {
        // Distribuição flex horizontal (Row)
        let total_flex: i32 = children.iter().map(|c| c.flex_weight()).sum();
        let fixed_w: i32 = children.iter()
            .filter(|c| c.flex_weight() == 0)
            .map(|c| {
                let s = cache.measure_or_compute(c.as_ref(), constraints, theme);
                child_slot += 1;
                s.w
            })
            .sum();
        let remaining = (parent_rect.width() - fixed_w).max(0);

        // Segunda passagem: aloca rects
        child_slot = slot_start + 1;
        let mut x = parent_rect.x0;
        for child in children {
            let idx = child_slot;
            let s = cache.measure_or_compute(child.as_ref(), constraints, theme);
            let child_w = if child.flex_weight() > 0 && total_flex > 0 {
                remaining * child.flex_weight() / total_flex
            } else {
                s.w
            };
            let child_h = s.h;
            let child_rect = Rect::from_xywh(x, parent_rect.y0, child_w, child_h);
            layout_tree(child.as_ref(), Constraints::tight(child_w, child_h), theme, cache, child_rect, idx);
            x += child_w + widget.gap();
            child_slot += 1 + count_subtree_slots(child.as_ref());
        }
    }
}

/// Desenha a árvore usando rects cacheados.
pub fn draw_tree(
    widget: &dyn Widget,
    cache: &UiCache,
    slot: usize,
    c: &mut Canvas,
    theme: &Theme,
) {
    let Some(rect) = cache.rect(slot) else { return };
    let saved = c.enter(rect);
    widget.draw(c, rect, theme);
    let mut child_slot = slot + 1;
    for child in widget.children() {
        draw_tree(child.as_ref(), cache, child_slot, c, theme);
        child_slot += 1 + count_subtree_slots(child.as_ref());
    }
    c.restore(saved);
}
```

---

## Camada 4: Slot Table — Estado Local Persistente

```rust
// crates/symbian-decl-ui/src/slot.rs

pub struct SlotTable {
    slots: Vec<SlotEntry>,
    cursor: usize,
    generation: u32,
}

enum SlotEntry {
    State(Box<dyn Any>),
    Group(u32),
}

impl SlotTable {
    pub fn new() -> Self {
        Self { slots: Vec::new(), cursor: 0, generation: 0 }
    }

    pub fn begin_frame(&mut self) {
        self.cursor = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn use_state<T: 'static>(&mut self, initial: T) -> &mut T {
        if self.cursor < self.slots.len() {
            let entry = &mut self.slots[self.cursor];
            match entry {
                SlotEntry::State(data) => {
                    self.cursor += 1;
                    data.downcast_mut::<T>()
                        .expect("use_state type mismatch: same call-site, different T")
                }
                SlotEntry::Group(_) => panic!("use_state at a group slot"),
            }
        } else {
            self.slots.push(SlotEntry::State(Box::new(initial)));
            self.cursor += 1;
            match self.slots.last_mut().unwrap() {
                SlotEntry::State(data) => data.downcast_mut::<T>().unwrap(),
                _ => unreachable!(),
            }
        }
    }

    pub fn begin_group(&mut self, key: u32) {
        self.slots.push(SlotEntry::Group(key));
        self.cursor += 1;
    }
}
```

O slot table é criado uma vez no `App` e passado via `&mut` para cada view. A cada frame, `begin_frame()` reseta o cursor e o view repovoa os slots na mesma ordem de chamada. Se um componente condicionalmente não é renderizado, seus slots são pulados.

---

## Camada 5: MVU — Model, Msg, Cmd, App

### `Cmd` — efeitos como valores

```rust
// crates/symbian-decl-ui/src/cmd.rs

pub enum Cmd {
    None,
    Exit,
    SetTimer { handle: i32, ms: u32 },
    Connect { host: &'static str, port: u16 },
    Send { socket: i32, data: &'static [u8] },
    PushScreen(ScreenId),
    PopScreen,
}
```

### `DeclarativeApp` trait

```rust
// crates/symbian-decl-ui/src/app.rs

pub trait DeclarativeApp {
    type Model;
    type Message: Clone;

    const TITLE: &'static str;

    fn init() -> Self::Model;
    fn update(model: &mut Self::Model, msg: Self::Message) -> Cmd;
    fn view(model: &Self::Model, cache: &mut UiCache, slots: &mut SlotTable) -> Box<dyn Widget>;
}
```

### Bridge para `symbian_ui::App`

```rust
// crates/symbian-decl-ui/src/bridge.rs

pub struct DeclarativeAppBridge<A: DeclarativeApp> {
    inner: A,
    model: A::Model,
    cache: UiCache,
    slots: SlotTable,
    exit_requested: bool,
}

impl<A: DeclarativeApp> symbian_ui::App for DeclarativeAppBridge<A> {
    fn handle_key(&mut self, ev: KeyEvent, _theme: &Theme, _screen: Rect) -> Handled {
        // Conversão de KeyEvent → A::Message (definida pelo usuário)
        let msg = A::Message::from_key_event(ev);
        let cmd = A::update(&mut self.model, msg);
        self.execute(cmd);
        Handled::Consumed
    }

    fn draw(&mut self, c: &mut Canvas, theme: &Theme) {
        self.cache.begin_frame();
        self.slots.begin_frame();
        let root = A::view(&self.model, &mut self.cache, &mut self.slots);
        let constraints = Constraints::tight(320, 240);
        measure_tree(root.as_ref(), constraints, theme, &mut self.cache);
        layout_tree(root.as_ref(), constraints, theme, &mut self.cache, Rect::from_size(c.size()), 0);
        draw_tree(root.as_ref(), &self.cache, 0, c, theme);
    }

    fn should_exit(&self) -> bool { self.exit_requested }
    fn title(&self) -> &str { A::TITLE }
}
```

### Exemplo de App com MVU

```rust
// apps/telegram/src/declarative.rs (futuro)

pub enum AppMsg {
    Key(KeyEvent),
    TimerFired(i32),
    DialogsLoaded(Vec<Chat>),
}

pub struct AppModel {
    pub chats: Vec<Chat>,
    pub selected: usize,
    pub scroll: i32,
    pub loading: bool,
    pub screen: ScreenId,
}

impl DeclarativeApp for TelegramApp {
    type Model = AppModel;
    type Message = AppMsg;
    const TITLE: &'static str = "Telegram";

    fn init() -> AppModel {
        AppModel { chats: Vec::new(), selected: 0, scroll: 0, loading: false, screen: ScreenId::ChatList }
    }

    fn update(model: &mut AppModel, msg: AppMsg) -> Cmd {
        match msg {
            AppMsg::Key(KeyEvent { key: Key::Down, .. }) => {
                model.selected = (model.selected + 1).min(model.chats.len().saturating_sub(1));
                Cmd::None
            }
            AppMsg::Key(KeyEvent { key: Key::Softkey(Softkey::Right), .. }) => Cmd::Exit,
            AppMsg::Key(KeyEvent { key: Key::Select, .. }) => {
                if !model.chats.is_empty() {
                    Cmd::PushScreen(ScreenId::Conversation(model.selected))
                } else {
                    Cmd::None
                }
            }
            AppMsg::DialogsLoaded(chats) => { model.chats = chats; Cmd::None }
            _ => Cmd::None,
        }
    }

    fn view(model: &AppModel, cache: &mut UiCache, slots: &mut SlotTable) -> Box<dyn Widget> {
        match model.screen {
            ScreenId::ChatList => Box::new(chat_list_screen(model)),
            ScreenId::Conversation(i) => Box::new(conversation_screen(model, i)),
        }
    }
}
```

---

## Catálogo de Widgets Embutidos

```
symbian-decl-ui/src/
├── lib.rs              re-exports
├── widget.rs           Widget trait + WidgetHash
├── cache.rs            UiCache
├── layout.rs           measure_tree, layout_tree, draw_tree
├── length.rs           Length enum
├── constraints.rs      Constraints struct
├── cmd.rs              Cmd enum
├── app.rs              DeclarativeApp trait
├── bridge.rs           Bridge para symbian_ui::App
├── slot.rs             SlotTable (use_state, begin_group)
├── theme.rs            FontRole enum
└── widgets/
    ├── mod.rs
    ├── screen.rs       Screen builder
    ├── row.rs          Row builder + Widget impl
    ├── column.rs       Column builder + Widget impl
    ├── text.rs         Text builder + Widget impl
    ├── scroll_list.rs  ScrollList + virtualização + ListState interno
    ├── title_bar.rs    TitleBar (delega p/ chrome::title_bar)
    ├── softkey_bar.rs  SoftkeyBar (delega p/ chrome::softkey_bar)
    ├── avatar.rs       Avatar (delega p/ chrome::avatar)
    ├── badge.rs        Badge (delega p/ chrome::badge)
    ├── text_field.rs   TextField (delega p/ edit::TextField)
    ├── button.rs       Button (Row + Text + OnKey)
    ├── spacer.rs       Spacer
    ├── divider.rs      Divider
    └── on_key.rs       OnKey event handler
```

### Referência rápida de builders

```rust
// Screen
Screen::new()
    .title(TitleBar::new("Telegram").subtitle("conectado"))
    .content(row_or_column_or_scrolllist)
    .softkeys(SoftkeyBar::new().center("Abrir").right("Sair"))

// Row / Column
Row::new()
    .height(38)
    .background(theme.palette.bg)
    .padding(Edges::all(5))
    .gap(6)
    .child(Text::new("Nome"))
    .child(Text::new("14:32"))
    .optional(has_unread, || Badge::new(3))

// Text
Text::new("Mensagem")
    .font(FontRole::Body)
    .color(Color::hex(0xFFFFFF))
    .dim()                     // shorthand: theme.palette.dim
    .align(Align::End)
    .ellipsis(true)
    .max_lines(2)
    .flex(1)

// ScrollList
ScrollList::new(item_count, row_height)
    .selected(sel)
    .scroll(scroll_offset)
    .scrollbar(true)
    .row(|idx, selected, slots| {
        Row::new().height(38)
            .child(Text::new(&chats[idx].name))
            .child(Text::new(&chats[idx].time).flex(1).align(End))
    })

// TextField
TextField::new()
    .value(&mut text)
    .placeholder("Digite...")
    .masked(true)
    .max_length(128)
    .on_submit(|text| { /* enviar */ })

// Outros
Avatar::new(initials, color_seed).size(30)
Badge::new(count).fill(accent).text_color(white)
Button::new("Enviar").on_click(|| { }).accent().disabled(cond)
Spacer::new().width(5)
Divider::new().color(theme.palette.divider)
OnKey::new(Key::Select, || { /* handler */ })
```

---

## Plano de Fases

### Fase 0 — Skeleton (1 dia)

**Arquivos:**
- `crates/symbian-decl-ui/Cargo.toml`
- `crates/symbian-decl-ui/src/lib.rs`
- `crates/symbian-decl-ui/src/widget.rs`
- `crates/symbian-decl-ui/src/length.rs`
- `crates/symbian-decl-ui/src/constraints.rs`

**Tarefas:**
- Criar o crate, adicionar ao workspace `Cargo.toml`
- Definir `Widget` trait com defaults para todos os métodos
- Definir `Length`, `Constraints`
- Widget dummy `Spacer` que implementa o trait

**Testes:**
- `Spacer::new().width(10).measure(tight(100, 50))` retorna Size(10, 0)
- Widget compila com `no_std` + `alloc`

### Fase 1 — Layout Engine (3 dias)

**Arquivos:**
- `crates/symbian-decl-ui/src/cache.rs`
- `crates/symbian-decl-ui/src/layout.rs`
- `crates/symbian-decl-ui/src/widgets/row.rs`
- `crates/symbian-decl-ui/src/widgets/column.rs`
- `crates/symbian-decl-ui/src/widgets/screen.rs`

**Tarefas:**
- Implementar `UiCache` com `measure_or_compute()` e `set_rect()`
- Implementar `measure_tree()`, `layout_tree()`, `draw_tree()`
- Implementar `Row` builder + Widget impl com distribuição flex
- Implementar `Column` builder + Widget impl (análogo, eixo Y)
- Implementar `Screen` builder com `.title()`, `.content()`, `.softkeys()`

**Testes:**
- `Row::new().child(Text("A")).child(Text("B")).measure()` retorna altura correta
- Frame 2 com mesmos dados: `measure()` não é chamada (cache hit)
- `Row::new().child(Text(flex(1))).child(Text(flex(1)))` distribui 50/50

### Fase 2 — Widgets de Chrome (2 dias)

**Arquivos:**
- `crates/symbian-decl-ui/src/theme.rs`
- `crates/symbian-decl-ui/src/widgets/title_bar.rs`
- `crates/symbian-decl-ui/src/widgets/softkey_bar.rs`
- `crates/symbian-decl-ui/src/widgets/text.rs`
- `crates/symbian-decl-ui/src/widgets/avatar.rs`
- `crates/symbian-decl-ui/src/widgets/badge.rs`
- `crates/symbian-decl-ui/src/widgets/spacer.rs`
- `crates/symbian-decl-ui/src/widgets/divider.rs`
- `crates/symbian-decl-ui/src/widgets/on_key.rs`

**Tarefas:**
- `TitleBar` — wrapper sobre `chrome::title_bar()`
- `SoftkeyBar` — wrapper sobre `chrome::softkey_bar()`
- `Text` — wrapper sobre `Canvas::draw_text_in()`
- `Avatar` — wrapper sobre `chrome::avatar()`
- `Badge` — wrapper sobre `chrome::badge()`
- `Spacer`, `Divider`, `OnKey` — widgets mínimos
- `FontRole` enum + `Fonts::resolve()`

**Testes:**
- Tela estática completa: TitleBar + 3 Rows de chat + SoftkeyBar
- Visualmente idêntico ao symbian-ui puro (abrir no simulador)

### Fase 3 — Slot Table (2 dias)

**Arquivos:**
- `crates/symbian-decl-ui/src/slot.rs`

**Tarefas:**
- Implementar `SlotTable` com `use_state::<T>()`
- Implementar `begin_frame()`, `begin_group(key)`
- Integrar `SlotTable` no `ScrollList`: cada row ganha slot próprio por key
- Testar: dois `TextField` irmãos com estado independente

**Testes:**
- Dois TextFields, digitar em cada um, alternar: estado preservado
- ScrollList com 20 rows, cada uma com seu próprio `use_state(String)`
- Remover metade das rows: slots órfãos não causam panic

### Fase 4 — MVU Core (2 dias)

**Arquivos:**
- `crates/symbian-decl-ui/src/cmd.rs`
- `crates/symbian-decl-ui/src/app.rs`
- `crates/symbian-decl-ui/src/bridge.rs`

**Tarefas:**
- `Cmd` enum com `None`, `Exit`, `SetTimer`, `PushScreen`, `PopScreen`
- `DeclarativeApp` trait com `Model`, `Message`, `init()`, `update()`, `view()`
- `DeclarativeAppBridge` que implementa `symbian_ui::App`
- App mínimo com 2 telas (ChatList + Detail) e navegação entre elas

**Testes:**
- `App::init()` → `update(Key(Select))` → `Cmd::PushScreen` → redraw mostra tela 2
- `update(Key(Softkey(Right)))` → `Cmd::Exit` → `should_exit()` retorna true
- `update()` é função pura: não toca em Canvas, não aloca

### Fase 5 — ScrollList + TextField + Button (2 dias)

**Arquivos:**
- `crates/symbian-decl-ui/src/widgets/scroll_list.rs`
- `crates/symbian-decl-ui/src/widgets/text_field.rs`
- `crates/symbian-decl-ui/src/widgets/button.rs`

**Tarefas:**
- `ScrollList` com `ListState` interno, virtualização, scrollbar
- `TextField` delegando para `edit::TextField`, suporte a masked + cursor
- `Button` como `Row + Text + OnKey`
- `.row()` builder que passa `&mut SlotTable` para cada row

**Testes:**
- ScrollList com 200 itens no simulador: scroll flúido, sem flicker
- TextField aceita texto, backspace, navegação de cursor
- Button dispara callback no Select

### Fase 6 — Telegram v2 (3 dias)

**Arquivos:**
- `apps/telegram/src/declarative/chats.rs`
- `apps/telegram/src/declarative/conv.rs`
- `apps/telegram/src/declarative/login.rs`
- `apps/telegram/Cargo.toml` — feature flag `declarative`

**Tarefas:**
- Reescrever `chats.rs` usando `symbian-decl-ui`
- Reescrever `conv.rs` (bubbles + composer)
- Reescrever `login.rs` (phone, code, password screens)
- Feature flag: `#[cfg(feature = "declarative")]` seleciona a implementação
- Comparar binário: tamanho, performance vs versão imperativa
- Testes de regressão no simulador

**Testes:**
- `cargo run --bin preview`: todas as telas funcionam
- Testes unitários passam em ambas as versões
- Binário ≤ 5% maior que versão imperativa

### Fase 7 — Tooling (1 dia)

**Arquivos:**
- `tools/symnew` — atualizado
- `tools/preview` — compatível com symbian-decl-ui
- `docs/plan-declarative-ui-sdk.md` — este documento

**Tarefas:**
- `tools/symnew declarative-app` gera projeto com `symbian-decl-ui`
- Preview tool funciona com apps declarativos
- Documentação de migração no README

---

## Métricas de Validação

| Fase | Como testar | Critério de sucesso |
|------|-------------|---------------------|
| F0 | `cargo test -p symbian-decl-ui` | Compila + 1 teste passa |
| F1 | `Row::new().child(Text).child(Text).measure()` loop 1000× | Frame 2+: 0 chamadas a measure() |
| F2 | Desenhar tela estática no simulador | Visualmente idêntico ao symbian-ui puro |
| F3 | Dois TextFields com estado local, alternar | Estado preservado, sem leak |
| F4 | App com ChatList + Conversation, navegar | Transições corretas, should_exit funciona |
| F5 | ScrollList com 200 itens | Scroll fluido, sem flicker, sem alocação por frame |
| F6 | `cargo run --bin preview` com Telegram | Funcionalmente idêntico ao atual, binário ≤ +5% |
| F7 | `tools/symnew declarative-app && cargo build` | Projeto gerado compila e mostra tela |

**Total estimado**: 13-15 dias de desenvolvimento.

---

## Estrutura de Diretórios Final

```
crates/symbian-decl-ui/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── widget.rs           Widget trait, WidgetHash
    ├── cache.rs            UiCache (measure + rect cache)
    ├── layout.rs           measure_tree, layout_tree, draw_tree
    ├── length.rs           Length enum
    ├── constraints.rs      Constraints struct
    ├── cmd.rs              Cmd enum
    ├── app.rs              DeclarativeApp trait
    ├── bridge.rs           DeclarativeAppBridge → symbian_ui::App
    ├── slot.rs             SlotTable, use_state, begin_group
    ├── theme.rs            FontRole enum
    └── widgets/
        ├── mod.rs
        ├── screen.rs
        ├── row.rs
        ├── column.rs
        ├── text.rs
        ├── scroll_list.rs
        ├── title_bar.rs
        ├── softkey_bar.rs
        ├── avatar.rs
        ├── badge.rs
        ├── text_field.rs
        ├── button.rs
        ├── spacer.rs
        ├── divider.rs
        └── on_key.rs
```
