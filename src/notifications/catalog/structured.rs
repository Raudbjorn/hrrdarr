//! Structured-event and mail-API providers, plus the custom script entry.
use super::{BOTH, Build, Config, Implementation, clip};
use crate::api::MediaDomain;
use crate::notifications::{
    events::{EventKind, Message},
    model::{Field, ImplementationKind},
    transport::{Body, Method, Outgoing, basic_auth},
};
use serde_json::json;

fn webhook(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let method = if c.text("method") == "PUT" {
        Method::Put
    } else {
        Method::Post
    };
    let mut request = Outgoing {
        method,
        url: c.text("url").to_owned(),
        headers: Vec::new(),
        body: Body::Json(m.json.clone()),
    };
    if let Some(user) = c.opt("username") {
        request = request.header("Authorization", basic_auth(user, c.text("password")));
    }
    Ok(vec![request])
}

fn apprise_type(m: &Message) -> &'static str {
    match m.kind {
        EventKind::HealthIssue => {
            if m.severity == Some(3) {
                "failure"
            } else {
                "warning"
            }
        }
        EventKind::ManualInteraction => "warning",
        EventKind::Import | EventKind::Add | EventKind::Test | EventKind::HealthRestored => {
            "success"
        }
        _ => "info",
    }
}
fn apprise(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let base = c.text("server_url").trim_end_matches('/').to_owned();
    let mut body = json!({
        "title": clip(&m.title, 250),
        "body": clip(&m.text, 3500),
        "type": apprise_type(m),
    });
    let url = if let Some(key) = c.opt("configuration_key") {
        if let Some(tags) = c.opt("tags") {
            body["tag"] = json!(tags);
        }
        let mut url = url::Url::parse(&format!("{base}/notify")).map_err(|_| "invalid_url")?;
        url.path_segments_mut()
            .map_err(|_| "invalid_url")?
            .push(key);
        String::from(url)
    } else {
        body["urls"] = json!(c.text("stateless_urls"));
        format!("{base}/notify")
    };
    let mut request = Outgoing::post_json(url, body);
    if let Some(user) = c.opt("username") {
        request = request.header("Authorization", basic_auth(user, c.text("password")));
    }
    Ok(vec![request])
}

fn notifiarr(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let app = match m.domain {
        MediaDomain::Tv => "sonarr",
        MediaDomain::Movies => "radarr",
    };
    Ok(vec![
        Outgoing::post_json(
            format!("https://notifiarr.com/api/v1/notification/{app}"),
            m.json.clone(),
        )
        .header("X-API-Key", c.text("api_key")),
    ])
}

fn signal(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let mut request = Outgoing::post_json(
        format!("{}/v2/send", c.text("server_url").trim_end_matches('/')),
        json!({
            "message": clip(&format!("{}\n{}", m.title, m.text), 3500),
            "number": c.text("sender_number"),
            "recipients": c.list("receivers"),
        }),
    );
    if let Some(user) = c.opt("username") {
        request = request.header("Authorization", basic_auth(user, c.text("password")));
    }
    Ok(vec![request])
}

fn sendgrid(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let to: Vec<_> = c
        .list("recipients")
        .into_iter()
        .map(|e| json!({"email": e}))
        .collect();
    Ok(vec![
        Outgoing::post_json(
            "https://api.sendgrid.com/v3/mail/send",
            json!({
                "personalizations": [{"to": to}],
                "from": {"email": c.text("from"), "name": c.opt("from_name")},
                "subject": clip(&m.title, 250),
                "content": [{"type": "text/plain", "value": clip(&m.text, 10000)}],
            }),
        )
        .header("Authorization", format!("Bearer {}", c.text("api_key"))),
    ])
}

fn mailgun(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let host = if c.bool("use_eu_endpoint") {
        "api.eu.mailgun.net"
    } else {
        "api.mailgun.net"
    };
    let mut url = url::Url::parse(&format!("https://{host}/v3")).map_err(|_| "invalid_url")?;
    url.path_segments_mut()
        .map_err(|_| "invalid_url")?
        .push(c.text("sender_domain"))
        .push("messages");
    let mut form = vec![
        ("from".to_owned(), c.text("from").to_owned()),
        ("subject".to_owned(), clip(&m.title, 250)),
        ("text".to_owned(), clip(&m.text, 10000)),
    ];
    form.extend(
        c.list("recipients")
            .into_iter()
            .map(|r| ("to".to_owned(), r)),
    );
    Ok(vec![Outgoing::post_form(String::from(url), form).header(
        "Authorization",
        basic_auth("api", c.text("api_key")),
    )])
}

pub fn implementations() -> Vec<Implementation> {
    vec![
        Implementation {
            name: "webhook",
            label: "Webhook",
            description: "POST (or PUT) the full structured event as JSON.",
            kind: ImplementationKind::Structured,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("url", "URL").required(),
                    Field::select("method", "Method", "POST", &["POST", "PUT"]),
                    Field::text("username", "Username"),
                    Field::secret("password", "Password"),
                ]
            },
            build: Build::Http(webhook),
        },
        Implementation {
            name: "apprise",
            label: "Apprise",
            description: "Relay through an Apprise API server.",
            kind: ImplementationKind::Message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("server_url", "Apprise API URL").required(),
                    Field::text("configuration_key", "Configuration key")
                        .help("Persistent key; leave empty to use stateless URLs."),
                    Field::secret("stateless_urls", "Stateless URLs")
                        .help("Comma or space separated Apprise URLs."),
                    Field::text("tags", "Tags"),
                    Field::text("username", "Username"),
                    Field::secret("password", "Password"),
                ]
            },
            build: Build::Http(apprise),
        },
        Implementation {
            name: "notifiarr",
            label: "Notifiarr",
            description: "Forward the structured event to Notifiarr (protocol not verified against the live service).",
            kind: ImplementationKind::Structured,
            domains: BOTH,
            fields: || vec![Field::secret("api_key", "API key").required()],
            build: Build::Http(notifiarr),
        },
        Implementation {
            name: "signal",
            label: "Signal",
            description: "Send through a signal-cli-rest-api server.",
            kind: ImplementationKind::Message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("server_url", "Server URL").required(),
                    Field::text("sender_number", "Sender number").required(),
                    Field::text("receivers", "Receivers")
                        .required()
                        .help("Comma separated numbers or group IDs."),
                    Field::text("username", "Username"),
                    Field::secret("password", "Password"),
                ]
            },
            build: Build::Http(signal),
        },
        Implementation {
            name: "sendgrid",
            label: "SendGrid",
            description: "Send mail through the SendGrid API.",
            kind: ImplementationKind::Message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "API key").required(),
                    Field::text("from", "From address").required(),
                    Field::text("from_name", "From name"),
                    Field::text("recipients", "Recipients")
                        .required()
                        .help("Comma separated."),
                ]
            },
            build: Build::Http(sendgrid),
        },
        Implementation {
            name: "mailgun",
            label: "Mailgun",
            description: "Send mail through the Mailgun API.",
            kind: ImplementationKind::Message,
            domains: BOTH,
            fields: || {
                vec![
                    Field::secret("api_key", "API key").required(),
                    Field::text("sender_domain", "Sender domain").required(),
                    Field::text("from", "From address").required(),
                    Field::text("recipients", "Recipients")
                        .required()
                        .help("Comma separated."),
                    Field::boolean("use_eu_endpoint", "Use EU endpoint", false),
                ]
            },
            build: Build::Http(mailgun),
        },
        Implementation {
            name: "custom_script",
            label: "Custom Script",
            description: "Run a local program with the event in sonarr_* / radarr_* environment variables. No shell is used.",
            kind: ImplementationKind::Script,
            domains: BOTH,
            fields: || {
                vec![
                    Field::path("path", "Script path")
                        .required()
                        .help("Absolute path to an executable file."),
                    Field::text("arguments", "Arguments").help(
                        "Whitespace separated; double quotes group words. Passed without a shell.",
                    ),
                ]
            },
            build: Build::Script,
        },
    ]
}
