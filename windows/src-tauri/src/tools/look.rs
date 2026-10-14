// ARIA looks at the owner's screen: she flies out to the window they are working
// in (desktop.rs visit), and its picture comes back to her as an image. Hers
// only — never on the Grok Bots' route — and the picture lives in this turn
// alone (agent.rs drops tool images once the turn is over).

use serde_json::json;
use tauri::AppHandle;

use super::{schema, Outcome};
use crate::policy;
use crate::providers::{Part, ToolSpec};

pub const NAME: &str = "look_at_screen";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: NAME.into(),
        description: "Look at the window the owner is working in: you fly to it on the desktop and get a picture \
of it. Use it when they ask what is on their screen, about an error or a page they have open, or when seeing it \
would help you answer. You see one window, the one they used last before talking to you."
            .into(),
        schema: schema(json!({}), &[]),
    }
}

pub async fn run(app: &AppHandle) -> Outcome {
    let Ok(Some(look)) = tauri::async_runtime::spawn_blocking(crate::context::look).await else {
        policy::audit(NAME, "read", "nothing to see");
        return Outcome::err("There is no window to look at (everything is minimised, or only ARIA is open).");
    };
    policy::audit(NAME, "read", &format!("{} — {}", look.app_name, look.title));
    crate::desktop::visit(app, look.frame).await;
    let mut out = Outcome::ok(format!(
        "You looked at the window of {} titled \"{}\". Its picture is attached.",
        look.app_name, look.title
    ));
    out.images.push(Part::Image { media: "image/png".into(), data: crate::media::base64(&look.png) });
    out
}
