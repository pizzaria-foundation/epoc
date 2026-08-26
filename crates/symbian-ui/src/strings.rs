//! The words every interface uses, in both languages.
//!
//! This table is deliberately small and deliberately generic. It holds the words that appear on a
//! softkey bar or a dialog button in *any* application — the ones that would otherwise be typed out
//! again in each one, and get translated slightly differently each time.
//!
//! Anything an application says about its own subject belongs in that application's own table.
//! `Save` is here; `Save this appointment` is not, and neither is `No towers in range`.
//!
//! # Why this exists at all
//!
//! `modal.rs` used to open with `action_label: String::from("Escolher")`, and its own documentation
//! said why that was a problem better than this can:
//!
//! > a shared widget that carries a language hands every later caller a decision it never asked
//! > about, and the symptom is one Portuguese word in an otherwise English screen — which is
//! > exactly what the boot manager's first confirmation dialog looked like.
//!
//! The decision is now the phone's, which is the only party that knows the answer.

crate::strings! {
    /// The affirmative softkey. `Modal`'s default, and what a confirmation dialog commits with.
    select = { en: "Select", pt: "Escolher" },
    /// Leave without changing anything. The left softkey on almost every screen in this project.
    back = { en: "Back", pt: "Voltar" },
    /// Abandon an operation in progress, which is not the same as `back` — one undoes, the other
    /// declines. Two words in English and two in Portuguese, kept apart because a dialog that
    /// offers "Back" where it means "Cancel" reads as though nothing was going to happen.
    cancel = { en: "Cancel", pt: "Cancelar" },
    /// Commit an edit.
    save = { en: "Save", pt: "Salvar" },
    /// The right softkey that opens a menu.
    options = { en: "Options", pt: "Opções" },
    /// Close the application. The platform's own word for the right softkey on a root screen.
    exit = { en: "Exit", pt: "Sair" },
    /// Enter the thing under the cursor.
    open = { en: "Open", pt: "Abrir" },
    /// Dismiss an overlay, leaving what is behind it as it was.
    close = { en: "Close", pt: "Fechar" },
    /// Destroy something, and the word a confirmation asks about.
    remove = { en: "Remove", pt: "Remover" },
    /// Change the thing under the cursor. Distinct from `open`: one looks, the other alters.
    edit = { en: "Edit", pt: "Editar" },
    /// Create one more of whatever the list holds.
    add = { en: "Add", pt: "Adicionar" },
    /// Throw the thing away. Portuguese has two words here and they are not interchangeable:
    /// `Remover` takes something out of a list, `Apagar` destroys it. English uses `Delete` for the
    /// second, which is why this is not a synonym of `remove` above.
    delete = { en: "Delete", pt: "Apagar" },
    /// The two answers to a question. Short enough that a dialog can put them on both softkeys.
    yes = { en: "Yes", pt: "Sim" },
    no = { en: "No", pt: "Não" },
    /// A list with nothing in it. `chrome::placeholder`'s default, and the one string here that is
    /// a sentence rather than a word.
    nothing_here = { en: "Nothing here", pt: "Nada aqui" },
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbian_sys::Lang;

    #[test]
    fn no_entry_was_filled_in_by_copying_the_english() {
        // The way a table like this actually goes wrong: somebody adds a row, needs it to compile,
        // and puts the English in the `pt` slot meaning to come back. Every word here differs
        // between the two languages, so a copy stands out — and this asserts it rather than hoping
        // a reviewer notices.
        let entries: &[fn() -> &'static str] = &[
            select, back, cancel, save, options, exit, open, close, remove, edit, add, delete,
            yes, no, nothing_here,
        ];
        crate::lang::set(Lang::En);
        let en: alloc::vec::Vec<&str> = entries.iter().map(|f| f()).collect();
        crate::lang::set(Lang::Pt);
        let pt: alloc::vec::Vec<&str> = entries.iter().map(|f| f()).collect();
        crate::lang::set(Lang::En);

        for (i, (e, p)) in en.iter().zip(pt.iter()).enumerate() {
            assert_ne!(e, p, "entry {i} is the same in both languages: {e:?}");
        }
    }

    #[test]
    fn back_and_cancel_are_different_words() {
        // They mean different things — one undoes, the other declines — and the cheapest way for
        // that distinction to be lost is for somebody to decide they are synonyms while
        // translating. Checked in both languages because they could collide in either.
        for l in [Lang::En, Lang::Pt] {
            crate::lang::set(l);
            assert_ne!(back(), cancel(), "{l:?}");
        }
        crate::lang::set(Lang::En);
    }
}
