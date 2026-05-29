use crate::api::{Command, CommandEnvelope};
use leptos::prelude::*;
use taskagent_domain::{Actor, NewTask};
use uuid::Uuid;

#[component]
pub fn Composer() -> impl IntoView {
    let input_value = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let error_msg: RwSignal<Option<String>> = RwSignal::new(None);

    let on_keydown = move |e: web_sys::KeyboardEvent| {
        if e.key() != "Enter" {
            return;
        }
        let text = input_value.get_untracked().trim().to_string();
        if text.is_empty() {
            return;
        }

        loading.set(true);
        error_msg.set(None);

        wasm_bindgen_futures::spawn_local(async move {
            let result = submit_text(&text).await;
            loading.set(false);
            match result {
                Ok(()) => {
                    input_value.set(String::new());
                }
                Err(msg) => {
                    error_msg.set(Some(msg));
                }
            }
        });
    };

    view! {
        <div class="composer">
            <input
                type="text"
                class="composer-input"
                placeholder="Describe a task… (AI parse on Enter)"
                autocomplete="off"
                prop:value=move || input_value.get()
                prop:disabled=move || loading.get()
                on:input=move |e| {
                    use wasm_bindgen::JsCast;
                    if let Some(input) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) {
                        input_value.set(input.value());
                    }
                }
                on:keydown=on_keydown
            />
            { move || if loading.get() {
                view! { <span class="composer-status">"…"</span> }.into_any()
            } else {
                view! { <span></span> }.into_any()
            }}
            { move || error_msg.get().map(|msg| view! {
                <span class="composer-status" style="color: var(--p0);">{msg}</span>
            })}
        </div>
    }
}

async fn submit_text(text: &str) -> Result<(), String> {
    // Try AI parse first, fall back to direct CreateTask
    let commands: Vec<Command> = match crate::api::ai_parse(text).await {
        Ok(cmd) => vec![cmd],
        Err(_) => {
            // AI unavailable — plain CreateTask
            vec![Command::CreateTask {
                task: NewTask::new(text),
            }]
        }
    };

    for cmd in commands {
        let envelope = CommandEnvelope {
            command: cmd,
            actor: Actor::User,
            client_command_id: Some(Uuid::new_v4()),
        };
        crate::api::dispatch_command(&envelope)
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
