use crate::{
    component,
    components::ContextProvider,
    element,
    hooks::{create_keybinding_context, parse_keybinding_blocks, KeybindingBlock},
    AnyElement, Context, Hooks, Props,
};

/// Props for [`KeybindingProvider`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct KeybindingProviderProps<'a> {
    /// Keybinding blocks to parse and expose to descendants.
    pub bindings: Vec<KeybindingBlock>,
    /// Children that should use action-based keybindings.
    pub children: Vec<AnyElement<'a>>,
}

/// Provides CC-style action keybindings with contexts and chords.
///
/// Descendants use [`UseActionKeybindings`](crate::hooks::UseActionKeybindings)
/// to register action handlers, and optionally call
/// `hooks.use_register_keybinding_context("Context", true)` while a context is
/// active. Chords such as `ctrl+x ctrl+k` are resolved by the provider, with
/// longer chord prefixes taking precedence over single-key matches.
#[component]
pub fn KeybindingProvider<'a>(
    mut hooks: Hooks,
    props: &mut KeybindingProviderProps<'a>,
) -> impl Into<AnyElement<'a>> {
    let parsed = parse_keybinding_blocks(&props.bindings);
    let keybindings = create_keybinding_context(&mut hooks, parsed);

    element! {
        ContextProvider(value: Context::owned(keybindings)) {
            #(props.children.iter_mut())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use futures::{stream, StreamExt};

    fn key(ch: char) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch)))
    }

    fn ctrl(ch: char) -> TerminalEvent {
        let mut event = KeyEvent::new(KeyEventKind::Press, KeyCode::Char(ch));
        event.modifiers = KeyModifiers::CONTROL;
        TerminalEvent::Key(event)
    }

    #[component]
    fn PersistentChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut count = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(_)) {
                count += 1;
            }
        });
        if count.get() > 0 {
            system.exit();
        }
        element!(Text(content: format!("count={}", count.get())))
    }

    #[test]
    fn keybinding_provider_keeps_children_across_rerenders() {
        let canvases: Vec<_> = smol::block_on(
            element! {
                KeybindingProvider(bindings: Vec::new()) {
                    PersistentChild
                }
            }
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![key(
                'x',
            )])))
            .collect(),
        );

        assert_eq!(
            canvases.iter().map(Canvas::to_string).collect::<Vec<_>>(),
            vec!["count=0\n".to_string(), "count=1\n".to_string()]
        );
    }

    #[component]
    fn ChordChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut log = hooks.use_state(String::new);

        hooks.use_action_keybinding("app:quit", move || {});
        hooks.use_action_keybinding("app:noop", move || {});
        hooks.use_action_keybinding_with_options(
            "chat:killAgents",
            ActionKeybindingOptions {
                context: "Chat".to_string(),
                active: true,
            },
            move || {
                log.set("chord fired".to_string());
                true
            },
        );

        let current = log.read().clone();
        if current == "chord fired" {
            system.exit();
        }
        element!(Text(content: current))
    }

    #[test]
    fn action_keybinding_chord_survives_earlier_global_handlers() {
        let bindings = vec![
            KeybindingBlock::new(
                "Global",
                vec![
                    KeybindingEntry::new("q", "app:quit"),
                    KeybindingEntry::new("ctrl+n", "app:noop"),
                ],
            ),
            KeybindingBlock::new(
                "Chat",
                vec![KeybindingEntry::new("ctrl+x ctrl+k", "chat:killAgents")],
            ),
        ];

        let canvases: Vec<_> = smol::block_on(
            element! {
                KeybindingProvider(bindings) {
                    ChordChild
                }
            }
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                ctrl('x'),
                ctrl('k'),
            ])))
            .collect(),
        );

        assert_eq!(canvases.last().unwrap().to_string(), "chord fired\n");
    }
}
