//! Chat and push-message providers. Each builder is a pure function from settings and a message
//! to requests, so the protocol is unit-testable without any I/O.
use super::{BOTH, Build, Config, Implementation, clip};
use crate::api::MediaDomain;
use crate::notifications::{
    events::{EventKind, Message},
    model::{Field, ImplementationKind},
    transport::{Outgoing, basic_auth},
};
use serde_json::json;

fn url_with_query(base: &str, query: &[(&str, &str)]) -> Result<String, &'static str> {
    let mut url = url::Url::parse(base).map_err(|_| "invalid_url")?;
    url.query_pairs_mut().extend_pairs(query);
    Ok(url.into())
}
fn join_path(base: &str, tail: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        tail.trim_start_matches('/')
    )
}

fn color(kind: EventKind) -> u32 {
    match kind {
        EventKind::Grab => 0xF5A623,
        EventKind::Import | EventKind::Add | EventKind::Test | EventKind::HealthRestored => {
            0x2ECC71
        }
        EventKind::Rename | EventKind::ApplicationUpdate => 0x3498DB,
        EventKind::HealthIssue | EventKind::Delete | EventKind::FileDelete => 0xE74C3C,
        EventKind::ManualInteraction => 0xF1C40F,
    }
}

fn discord(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut body = json!({
        "username": c.opt("username").unwrap_or("hrrdarr"),
        "embeds": [{
            "title": clip(&m.title, 256),
            "description": clip(&m.text, 4000),
            "color": color(m.kind),
        }],
    });
    if let Some(avatar) = c.opt("avatar_url") {
        body["avatar_url"] = json!(avatar);
    }
    Ok(vec![Outgoing::post_json(c.text("webhook_url"), body)])
}

fn slack(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut body = json!({
        "text": clip(&m.title, 300),
        "username": c.opt("username").unwrap_or("hrrdarr"),
        "attachments": [{
            "title": clip(&m.title, 300),
            "text": clip(&m.text, 3000),
            "fallback": clip(&format!("{}: {}", m.title, m.text), 3000),
            "color": format!("#{:06X}", color(m.kind)),
        }],
    });
    if let Some(icon) = c.opt("icon") {
        let key = if icon.starts_with("http://") || icon.starts_with("https://") {
            "icon_url"
        } else {
            "icon_emoji"
        };
        body[key] = json!(icon);
    }
    if let Some(channel) = c.opt("channel") {
        body["channel"] = json!(channel);
    }
    Ok(vec![Outgoing::post_json(c.text("webhook_url"), body)])
}

fn telegram(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    // sendMessage allows 4096 characters *after entity parsing*: `&amp;` counts as one, so the
    // escaped text fits whenever the clipped plain text (256 + 1 + 3500) does.
    let text = format!(
        "<b>{}</b>\n{}",
        html_escape::encode_text(&clip(&m.title, 256)),
        html_escape::encode_text(&clip(&m.text, 3500))
    );
    let mut body = json!({
        "chat_id": c.text("chat_id"),
        "text": text,
        "parse_mode": "HTML",
        "disable_notification": c.bool("send_silently"),
    });
    if let Some(topic) = c.int("topic_id") {
        body["message_thread_id"] = json!(topic);
    }
    Ok(vec![Outgoing::post_json(
        format!(
            "https://api.telegram.org/bot{}/sendMessage",
            c.text("bot_token")
        ),
        body,
    )])
}

fn ntfy(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let base = c.opt("server_url").unwrap_or("https://ntfy.sh");
    let priority: i64 = c.text("priority").parse().unwrap_or(3);
    let mut out = Vec::new();
    for topic in c.list("topics") {
        let mut body = json!({
            "topic": topic,
            "title": clip(&m.title, 250),
            "message": clip(&m.text, 3500),
            "priority": priority,
        });
        let tags = c.list("tags");
        if !tags.is_empty() {
            body["tags"] = json!(tags);
        }
        if let Some(click) = c.opt("click_url") {
            body["click"] = json!(click);
        }
        let mut request = Outgoing::post_json(base, body);
        if let Some(token) = c.opt("access_token") {
            request = request.header("Authorization", format!("Bearer {token}"));
        } else if let Some(user) = c.opt("username") {
            request = request.header("Authorization", basic_auth(user, c.text("password")));
        }
        out.push(request);
    }
    Ok(out)
}

fn gotify(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let body = json!({
        "title": clip(&m.title, 250),
        "message": clip(&m.text, 3500),
        "priority": c.int("priority").unwrap_or(5),
        "extras": {"client::display": {"contentType": "text/plain"}},
    });
    Ok(vec![
        Outgoing::post_json(join_path(c.text("server_url"), "message"), body)
            .header("X-Gotify-Key", c.text("app_token")),
    ])
}

fn pushover(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let priority: i64 = c.text("priority").parse().unwrap_or(0);
    let mut form = vec![
        ("token".to_owned(), c.text("api_token").to_owned()),
        ("user".to_owned(), c.text("user_key").to_owned()),
        ("title".to_owned(), clip(&m.title, 250)),
        ("message".to_owned(), clip(&m.text, 1000)),
        ("priority".to_owned(), priority.to_string()),
    ];
    let devices = c.list("devices");
    if !devices.is_empty() {
        form.push(("device".into(), devices.join(",")));
    }
    if let Some(sound) = c.opt("sound") {
        form.push(("sound".into(), sound.to_owned()));
    }
    if priority == 2 {
        form.push(("retry".into(), c.int("retry").unwrap_or(60).to_string()));
        form.push(("expire".into(), c.int("expire").unwrap_or(3600).to_string()));
    }
    Ok(vec![Outgoing::post_form(
        "https://api.pushover.net/1/messages.json",
        form,
    )])
}

fn pushbullet(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let auth = basic_auth(c.text("api_key"), "");
    let note = |extra: Option<(&str, String)>| {
        let mut body = json!({"type":"note","title":clip(&m.title,250),"body":clip(&m.text,3500)});
        if let Some((key, value)) = extra {
            body[key] = json!(value);
        }
        Outgoing::post_json("https://api.pushbullet.com/v2/pushes", body)
            .header("Authorization", auth.clone())
    };
    let devices = c.list("device_ids");
    let channels = c.list("channel_tags");
    if devices.is_empty() && channels.is_empty() {
        return Ok(vec![note(None)]);
    }
    let mut out: Vec<Outgoing> = devices
        .into_iter()
        .map(|d| note(Some(("device_iden", d))))
        .collect();
    out.extend(channels.into_iter().map(|t| note(Some(("channel_tag", t)))));
    Ok(out)
}

fn pushsafer(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut form = vec![
        ("k".to_owned(), c.text("private_key").to_owned()),
        ("d".to_owned(), c.opt("device").unwrap_or("a").to_owned()),
        ("t".to_owned(), clip(&m.title, 250)),
        ("m".to_owned(), clip(&m.text, 1000)),
        ("p".to_owned(), c.text("priority").to_owned()),
    ];
    if let Some(sound) = c.int("sound") {
        form.push(("s".into(), sound.to_string()));
    }
    if let Some(vibration) = c.int("vibration") {
        form.push(("v".into(), vibration.to_string()));
    }
    Ok(vec![Outgoing::post_form(
        "https://www.pushsafer.com/api",
        form,
    )])
}

fn simplepush(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut form = vec![
        ("key".to_owned(), c.text("key").to_owned()),
        ("title".to_owned(), clip(&m.title, 250)),
        ("msg".to_owned(), clip(&m.text, 1000)),
    ];
    if let Some(event) = c.opt("event") {
        form.push(("event".into(), event.to_owned()));
    }
    Ok(vec![Outgoing::post_form(
        "https://api.simplepush.io/send",
        form,
    )])
}

fn prowl(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    Ok(vec![Outgoing::post_form(
        "https://api.prowlapp.com/publicapi/add",
        vec![
            ("apikey".into(), c.text("api_key").to_owned()),
            ("application".into(), "hrrdarr".into()),
            ("event".into(), clip(&m.title, 1024)),
            ("description".into(), clip(&m.text, 10000)),
            ("priority".into(), c.text("priority").to_owned()),
        ],
    )])
}

fn join(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let title = clip(&m.title, 250);
    let text = clip(&m.text, 1000);
    let mut query = vec![
        ("apikey", c.text("api_key")),
        ("title", title.as_str()),
        ("text", text.as_str()),
        ("priority", c.opt("priority").unwrap_or("0")),
    ];
    let names = c.list("device_names").join(",");
    if names.is_empty() {
        query.push(("deviceId", "group.all"));
    } else {
        query.push(("deviceNames", names.as_str()));
    }
    Ok(vec![Outgoing::get(url_with_query(
        "https://joinjoaomgcd.appspot.com/_ah/api/messaging/v1/sendPush",
        &query,
    )?)])
}

fn pushcut(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut url =
        url::Url::parse("https://api.pushcut.io/v1/notifications").map_err(|_| "invalid_url")?;
    url.path_segments_mut()
        .map_err(|_| "invalid_url")?
        .push(c.text("notification_name"));
    Ok(vec![
        Outgoing::post_json(
            String::from(url),
            json!({"title":clip(&m.title,250),"text":clip(&m.text,1000)}),
        )
        .header("API-Key", c.text("api_key")),
    ])
}

fn priority_options() -> [&'static str; 5] {
    ["-2", "-1", "0", "1", "2"]
}

pub fn implementations() -> Vec<Implementation> {
    let message = ImplementationKind::Message;
    vec![
        Implementation {
            name: "discord",
            label: "Discord",
            description: "Post an embed to a Discord channel webhook.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret_url("webhook_url", "Webhook URL")
                        .required()
                        .help("Channel webhook; treated as a secret."),
                    Field::text("username", "Username").default_text("hrrdarr"),
                    Field::url("avatar_url", "Avatar URL"),
                ]
            },
            build: Build::Http(discord),
        },
        Implementation {
            name: "slack",
            label: "Slack",
            description: "Post to a Slack incoming webhook.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret_url("webhook_url", "Webhook URL")
                        .required()
                        .help("Incoming webhook; treated as a secret."),
                    Field::text("username", "Username").default_text("hrrdarr"),
                    Field::text("icon", "Icon (emoji or URL)"),
                    Field::text("channel", "Channel override"),
                ]
            },
            build: Build::Http(slack),
        },
        Implementation {
            name: "telegram",
            label: "Telegram",
            description: "Send a message through the Telegram Bot API.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("bot_token", "Bot token").required(),
                    Field::text("chat_id", "Chat ID").required(),
                    Field::number("topic_id", "Topic (thread) ID", 1, 2_147_483_647),
                    Field::boolean("send_silently", "Send silently", false),
                ]
            },
            build: Build::Http(telegram),
        },
        Implementation {
            name: "ntfy",
            label: "ntfy",
            description: "Publish to one or more ntfy topics.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("server_url", "Server URL").default_text("https://ntfy.sh"),
                    Field::text("topics", "Topics")
                        .required()
                        .help("Comma separated."),
                    Field::select("priority", "Priority", "3", &["1", "2", "3", "4", "5"]),
                    Field::text("tags", "Tags").help("Comma separated emoji short codes."),
                    Field::url("click_url", "Click URL"),
                    Field::text("username", "Username"),
                    Field::secret("password", "Password"),
                    Field::secret("access_token", "Access token"),
                ]
            },
            build: Build::Http(ntfy),
        },
        Implementation {
            name: "gotify",
            label: "Gotify",
            description: "Push to a Gotify server application.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("server_url", "Server URL").required(),
                    Field::secret("app_token", "Application token").required(),
                    Field::number("priority", "Priority", 0, 10).default_int(5),
                ]
            },
            build: Build::Http(gotify),
        },
        Implementation {
            name: "pushover",
            label: "Pushover",
            description: "Send a Pushover notification.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("user_key", "User key").required(),
                    Field::secret("api_token", "API token").required(),
                    Field::text("devices", "Devices").help("Comma separated; empty means all."),
                    Field::select("priority", "Priority", "0", &priority_options()),
                    Field::number("retry", "Retry seconds (emergency)", 30, 86_400).default_int(60),
                    Field::number("expire", "Expire seconds (emergency)", 30, 10_800)
                        .default_int(3600),
                    Field::text("sound", "Sound"),
                ]
            },
            build: Build::Http(pushover),
        },
        Implementation {
            name: "pushbullet",
            label: "Pushbullet",
            description: "Push a note to devices or channels.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "Access token").required(),
                    Field::text("device_ids", "Device IDs")
                        .help("Comma separated; empty with no channels means all."),
                    Field::text("channel_tags", "Channel tags").help("Comma separated."),
                ]
            },
            build: Build::Http(pushbullet),
        },
        Implementation {
            name: "pushsafer",
            label: "Pushsafer",
            description: "Send a Pushsafer notification (Radarr-only upstream).",
            kind: message,
            domains: &[MediaDomain::Movies],
            fields: || {
                vec![
                    Field::secret("private_key", "Private or alias key").required(),
                    Field::text("device", "Device or group ID").default_text("a"),
                    Field::select("priority", "Priority", "0", &priority_options()),
                    Field::number("sound", "Sound", 0, 62),
                    Field::number("vibration", "Vibration", 0, 3),
                ]
            },
            build: Build::Http(pushsafer),
        },
        Implementation {
            name: "simplepush",
            label: "Simplepush",
            description: "Send a Simplepush notification.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("key", "Key").required(),
                    Field::text("event", "Event"),
                ]
            },
            build: Build::Http(simplepush),
        },
        Implementation {
            name: "prowl",
            label: "Prowl",
            description: "Send a Prowl notification.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "API key").required(),
                    Field::select("priority", "Priority", "0", &priority_options()),
                ]
            },
            build: Build::Http(prowl),
        },
        Implementation {
            name: "join",
            label: "Join",
            description: "Push through Join (joaoapps).",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "API key").required(),
                    Field::text("device_names", "Device names")
                        .help("Comma separated; empty means all devices."),
                    Field::select("priority", "Priority", "0", &priority_options()),
                ]
            },
            build: Build::Http(join),
        },
        Implementation {
            name: "pushcut",
            label: "Pushcut",
            description: "Trigger a named Pushcut notification.",
            kind: message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "API key").required(),
                    Field::text("notification_name", "Notification name").required(),
                ]
            },
            build: Build::Http(pushcut),
        },
    ]
}
