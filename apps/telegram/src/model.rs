//! The data the UI draws.
//!
//! Deliberately protocol-free. These are the shapes the screens need, not the
//! shapes MTProto happens to return — so when the real client lands it fills these
//! in from `messages.getDialogs` / `messages.getHistory` and the UI does not move.
//! `mock()` is the stand-in until then.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Message {
    pub text: String,
    /// True when we sent it.
    pub outgoing: bool,
    /// Pre-formatted `HH:MM`. Formatting needs the device clock and the user's
    /// locale, neither of which belongs in the drawing path.
    pub time: String,
    pub state: Delivery,
}

/// Delivery state, shown as ticks on outgoing messages.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// Queued locally, not yet acknowledged by the server.
    Pending,
    Sent,
    Read,
    /// Never reached the server.
    Failed,
}

impl Delivery {
    pub fn glyph(self) -> &'static str {
        match self {
            Delivery::Pending => "\u{00B7}",
            Delivery::Sent => "\u{2713}",
            Delivery::Read => "\u{2713}\u{2713}",
            Delivery::Failed => "!",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Chat {
    pub name: String,
    pub time: String,
    pub unread: u32,
    /// Whether the last message in the preview is ours.
    pub last_outgoing: bool,
    pub messages: Vec<Message>,
}

impl Chat {
    /// One or two initials for the avatar, taken from word starts so "Ana Paula"
    /// gives "AP" rather than "An".
    pub fn initials(&self) -> String {
        let mut out = String::new();
        for word in self.name.split_whitespace().take(2) {
            if let Some(c) = word.chars().next() {
                out.extend(c.to_uppercase());
            }
        }
        if out.is_empty() {
            out.push('?');
        }
        out
    }

    /// Stable per-contact avatar tint. FNV-1a over the name, so it survives
    /// restarts and reordering without storing anything.
    pub fn color_seed(&self) -> u32 {
        let mut h: u32 = 0x811C_9DC5;
        for b in self.name.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    pub fn preview(&self) -> &str {
        self.messages.last().map(|m| m.text.as_str()).unwrap_or("")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pub chats: Vec<Chat>,
    /// What the title bar shows: connection state from the transport layer.
    pub status: String,
}

impl Store {
    /// Sample data, sized to expose the layout problems that matter: names that
    /// overflow, a preview that must ellipsize, a message long enough to wrap
    /// several lines, one short enough to under-fill a bubble, and Cyrillic.
    pub fn mock() -> Self {
        fn msg(text: &str, outgoing: bool, time: &str, state: Delivery) -> Message {
            Message { text: text.to_string(), outgoing, time: time.to_string(), state }
        }

        let mut chats = Vec::new();

        chats.push(Chat {
            name: "Ana Paula".to_string(),
            time: "14:32".to_string(),
            unread: 2,
            last_outgoing: false,
            messages: alloc::vec![
                msg("oi! conseguiu compilar aquilo?", false, "14:18", Delivery::Read),
                msg("consegui, o elf2e32 rodou de primeira em aarch64", true, "14:20", Delivery::Read),
                msg("sério? achei que ia precisar de wine pra tudo", false, "14:21", Delivery::Read),
                msg(
                    "não, o Martin Storsjö reescreveu as ferramentas todas em C++ nativo. \
                     makesis, signsis, rcomp, elf2e32 — tudo compila direto no Linux.",
                    true,
                    "14:24",
                    Delivery::Read,
                ),
                msg("isso muda tudo", false, "14:30", Delivery::Sent),
                msg("agora falta rodar no aparelho de verdade", false, "14:32", Delivery::Sent),
            ],
        });

        chats.push(Chat {
            name: "Symbian Revive".to_string(),
            time: "13:07".to_string(),
            unread: 17,
            last_outgoing: false,
            messages: alloc::vec![
                msg("anyone tried GCC 15 for arm-none-symbianelf?", false, "12:55", Delivery::Read),
                msg("binutils dropped the triple, you have to patch config.bfd", true, "13:02", Delivery::Sent),
                msg("that worked, thanks", false, "13:07", Delivery::Sent),
            ],
        });

        chats.push(Chat {
            name: "Дмитрий".to_string(),
            time: "11:48".to_string(),
            unread: 0,
            last_outgoing: true,
            messages: alloc::vec![
                msg("привет! как дела с телефоном?", false, "11:40", Delivery::Read),
                msg("всё работает, спасибо", true, "11:48", Delivery::Read),
            ],
        });

        chats.push(Chat {
            name: "Um Nome Bem Comprido Que Não Cabe".to_string(),
            time: "ter".to_string(),
            unread: 0,
            last_outgoing: false,
            messages: alloc::vec![msg(
                "este preview é longo o suficiente para precisar de reticências no fim",
                false,
                "09:12",
                Delivery::Read,
            )],
        });

        chats.push(Chat {
            name: "Build Bot".to_string(),
            time: "seg".to_string(),
            unread: 0,
            last_outgoing: false,
            messages: alloc::vec![
                msg("ok", false, "08:00", Delivery::Read),
                msg("falhou: libgcov não compila", false, "08:02", Delivery::Read),
            ],
        });

        chats.push(Chat {
            name: "Notas".to_string(),
            time: "dom".to_string(),
            unread: 0,
            last_outgoing: true,
            messages: alloc::vec![msg("EPOCSTACKSIZE 0x8000", true, "22:10", Delivery::Pending)],
        });

        chats.push(Chat {
            name: "Marina".to_string(),
            time: "sáb".to_string(),
            unread: 1,
            last_outgoing: false,
            messages: alloc::vec![msg("👍", false, "19:30", Delivery::Sent)],
        });

        Self { chats, status: "conectado".to_string() }
    }
}
