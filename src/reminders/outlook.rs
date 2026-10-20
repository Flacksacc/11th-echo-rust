use super::{
    model::Occurrence,
    store::{Account, Calendar},
};
use chrono::{DateTime, Duration, Utc};
use oauth2::{CsrfToken, PkceCodeChallenge};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SCOPE: &str = "openid profile offline_access https://graph.microsoft.com/Calendars.Read";
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not initialize Microsoft connection.".into())
}
pub fn validate_client(id: &str) -> Result<(), String> {
    if id.len() != 36
        || id.bytes().enumerate().any(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b != b'-'
            } else {
                !b.is_ascii_hexdigit()
            }
        })
    {
        return Err(
            "Enter the public Microsoft application (client) ID from your app registration.".into(),
        );
    }
    Ok(())
}
async fn json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(
            "Microsoft is throttling calendar requests. Echo will retry automatically.".into(),
        );
    }
    if !status.is_success() {
        return Err(if status == reqwest::StatusCode::UNAUTHORIZED {
            "Microsoft sign-in expired. Reconnect this account."
        } else {
            "Microsoft could not complete the request. Check account permissions and connectivity."
        }
        .into());
    }
    if response
        .content_length()
        .is_some_and(|n| n > 8 * 1024 * 1024)
    {
        return Err("Microsoft response exceeded its page budget.".into());
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Microsoft response was interrupted.")?
    {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("Microsoft response exceeded its page budget.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Microsoft returned an invalid response.".into())
}
fn graph_url(path: &str) -> Result<url::Url, String> {
    let url = if path.starts_with("https://") {
        url::Url::parse(path)
    } else {
        url::Url::parse(&format!("https://graph.microsoft.com/v1.0/{path}"))
    }
    .map_err(|_| "Invalid Microsoft calendar URL.")?;
    if url.scheme() != "https"
        || url.host_str() != Some("graph.microsoft.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with("/v1.0/")
    {
        return Err("Microsoft returned an unexpected calendar endpoint.".into());
    }
    Ok(url)
}
async fn get(path: &str, token: &str) -> Result<Value, String> {
    json(
        client()?
            .get(graph_url(path)?)
            .bearer_auth(token)
            .header("Prefer", "outlook.timezone=\"UTC\", IdType=\"ImmutableId\"")
            .send()
            .await
            .map_err(|_| "Microsoft calendar is offline. Cached reminders remain available.")?,
    )
    .await
}
async fn pages(path: &str, token: &str) -> Result<Vec<Value>, String> {
    let mut next = Some(path.to_owned());
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    while let Some(path) = next {
        if !seen.insert(path.clone()) || seen.len() > 1000 {
            return Err("Microsoft calendar pagination did not complete.".into());
        }
        let page = get(&path, token).await?;
        let rows = page["value"]
            .as_array()
            .ok_or("Microsoft returned an invalid calendar page.")?;
        if result.len() + rows.len() > 100000 {
            return Err(
                "Calendar exceeds the 100,000 occurrence sync budget. Select fewer calendars."
                    .into(),
            );
        }
        result.extend(rows.iter().cloned());
        next = page["@odata.nextLink"].as_str().map(str::to_owned);
    }
    Ok(result)
}
pub async fn refresh(account: &Account) -> Result<(String, Account), String> {
    validate_client(&account.client_id)?;
    let refresh = crate::settings::unprotect_secret(&account.refresh)?;
    let value = json(
        client()?
            .post("https://login.microsoftonline.com/common/oauth2/v2.0/token")
            .form(&[
                ("client_id", account.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("scope", SCOPE),
            ])
            .send()
            .await
            .map_err(|_| "Microsoft sign-in is offline.")?,
    )
    .await?;
    let token = value["access_token"]
        .as_str()
        .ok_or("Microsoft did not issue a calendar access token.")?
        .to_owned();
    let mut account = account.clone();
    if let Some(refresh) = value["refresh_token"].as_str() {
        account.refresh = crate::settings::protect_secret(refresh)?;
    }
    Ok((token, account))
}
pub async fn login(id: &str) -> Result<(Account, Vec<Calendar>), String> {
    validate_client(id)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Could not prepare Microsoft browser sign-in.")?;
    let redirect = format!(
        "http://localhost:{}/",
        listener
            .local_addr()
            .map_err(|_| "Could not prepare Microsoft sign-in.")?
            .port()
    );
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let state = CsrfToken::new_random();
    let mut authorize =
        url::Url::parse("https://login.microsoftonline.com/common/oauth2/v2.0/authorize").unwrap();
    authorize.query_pairs_mut().extend_pairs([
        ("client_id", id),
        ("response_type", "code"),
        ("redirect_uri", &redirect),
        ("response_mode", "query"),
        ("scope", SCOPE),
        ("state", state.secret()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("prompt", "select_account"),
    ]);
    webbrowser::open(authorize.as_str())
        .map_err(|_| "Could not open Microsoft browser sign-in.")?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    let code = loop {
        let (mut socket, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| "Microsoft sign-in timed out. Try Connect again.")?
            .map_err(|_| "Microsoft sign-in callback failed.")?;
        let mut bytes = vec![0u8; 8192];
        let size = tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut bytes))
            .await
            .map_err(|_| "Microsoft sign-in callback timed out.")?
            .map_err(|_| "Microsoft sign-in callback failed.")?;
        let request = String::from_utf8_lossy(&bytes[..size]);
        let target = request
            .lines()
            .next()
            .and_then(|s| s.split_whitespace().nth(1))
            .unwrap_or("");
        let url = url::Url::parse(&format!("http://localhost{target}"));
        let fields = url
            .ok()
            .map(|u| {
                u.query_pairs()
                    .into_owned()
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();
        let valid = fields.get("state") == Some(state.secret());
        let body = if valid {
            "Echo received your sign-in. You can close this browser tab."
        } else {
            "Invalid sign-in callback."
        };
        let response=format!("HTTP/1.1 {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",if valid{"200 OK"}else{"400 Bad Request"},body.len(),body);
        let _ = socket.write_all(response.as_bytes()).await;
        if !valid {
            continue;
        }
        if fields.contains_key("error") {
            return Err("Microsoft sign-in was declined or blocked. Check app registration and account access.".into());
        }
        if let Some(code) = fields.get("code") {
            break code.clone();
        }
    };
    let value = json(
        client()?
            .post("https://login.microsoftonline.com/common/oauth2/v2.0/token")
            .form(&[
                ("client_id", id),
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect),
                ("code_verifier", verifier.secret()),
                ("scope", SCOPE),
            ])
            .send()
            .await
            .map_err(|_| "Microsoft sign-in could not finish.")?,
    )
    .await?;
    let token = value["access_token"]
        .as_str()
        .ok_or("Microsoft did not return a calendar access token.")?;
    // OIDC UserInfo needs only the already-consented openid/profile scopes.
    let identity = json(
        client()?
            .get("https://graph.microsoft.com/oidc/userinfo")
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| "Could not identify the Microsoft account.")?,
    )
    .await?;
    let sub = identity["sub"]
        .as_str()
        .ok_or("Microsoft did not identify the signed-in account.")?;
    let account = Account {
        id: format!("{:x}", Sha256::digest(sub)),
        name: identity["name"]
            .as_str()
            .unwrap_or("Microsoft account")
            .into(),
        client_id: id.into(),
        refresh: crate::settings::protect_secret(
            value["refresh_token"]
                .as_str()
                .ok_or("Microsoft did not grant offline calendar access.")?,
        )?,
        status: "Connected".into(),
        last_sync: 0,
    };
    let calendars = calendars(&account, token).await?;
    Ok((account, calendars))
}
pub async fn calendars(a: &Account, token: &str) -> Result<Vec<Calendar>, String> {
    pages(
        "me/calendars?$select=id,name,isDefaultCalendar&$top=100",
        token,
    )
    .await?
    .into_iter()
    .map(|v| {
        let id = v["id"]
            .as_str()
            .ok_or("Microsoft returned an invalid calendar identity.")?;
        Ok(Calendar {
            key: format!("{}:{id}", a.id),
            id: id.into(),
            account: a.id.clone(),
            name: format!("{} · {}", a.name, v["name"].as_str().unwrap_or("Calendar")),
            enabled: v["isDefaultCalendar"] == true,
        })
    })
    .collect()
}
fn stamp(v: &Value) -> Result<i64, String> {
    let value = v["dateTime"]
        .as_str()
        .ok_or("Calendar event has no timestamp.")?;
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc3339(&format!("{value}Z")))
        .map(|d| d.timestamp())
        .map_err(|_| "Calendar event has an invalid UTC timestamp.".into())
}
fn parse(v: &Value, a: &Account, c: &Calendar) -> Result<Occurrence, String> {
    let id = v["id"].as_str().ok_or("Calendar event has no identity.")?;
    let uid = v["iCalUId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}:{id}", a.id));
    let all_day = if v["isAllDay"] == true {
        Some(
            v["start"]["dateTime"]
                .as_str()
                .ok_or("All-day event has no date.")?
                .chars()
                .take(10)
                .collect(),
        )
    } else {
        None
    };
    let mut title = v["subject"].as_str().unwrap_or("Untitled event").to_owned();
    title.truncate(
        title
            .char_indices()
            .nth(512)
            .map(|(i, _)| i)
            .unwrap_or(title.len()),
    );
    Ok(Occurrence {
        key: format!("{:x}", Sha256::digest(uid)),
        provider_id: id.into(),
        account: a.id.clone(),
        calendar: c.key.clone(),
        series: v["seriesMasterId"].as_str().unwrap_or("").into(),
        title,
        start: stamp(&v["start"])?,
        end: stamp(&v["end"])?,
        all_day,
        cancelled: v["isCancelled"] == true || v["responseStatus"]["response"] == "declined",
        link: v["webLink"].as_str().unwrap_or("").into(),
    })
}
pub async fn events(
    a: &Account,
    c: &Calendar,
    token: &str,
    now: i64,
) -> Result<Vec<Occurrence>, String> {
    let mut url = graph_url("me/calendars/")?;
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&c.id)
        .push("calendarView");
    let start = DateTime::<Utc>::from_timestamp(now, 0).ok_or("Invalid calendar clock.")?
        - Duration::days(1);
    let end = start + Duration::days(31);
    url.query_pairs_mut().extend_pairs([("startDateTime",start.to_rfc3339().as_str()),("endDateTime",end.to_rfc3339().as_str()),("$top","200"),("$select","id,iCalUId,subject,start,end,isAllDay,isCancelled,seriesMasterId,responseStatus,webLink,originalStartTimeZone")]);
    let rows = pages(url.as_str(), token).await?;
    let mut events = Vec::new();
    for value in rows {
        let mut event = parse(&value, a, c)?;
        fix_all_day(&mut event, &value, token).await?;
        events.push(event);
    }
    Ok(events)
}
async fn fix_all_day(event: &mut Occurrence, value: &Value, token: &str) -> Result<(), String> {
    if event.all_day.is_none() {
        return Ok(());
    }
    let zone = value["originalStartTimeZone"].as_str().unwrap_or("UTC");
    if zone.len() > 128 || zone.contains(['\r', '\n', '"']) {
        return Err("Calendar has an invalid source time zone.".into());
    }
    let mut url = graph_url("me/events/")?;
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&event.provider_id);
    url.query_pairs_mut().append_pair("$select", "start");
    let response = client()?
        .get(url)
        .bearer_auth(token)
        .header(
            "Prefer",
            format!("outlook.timezone=\"{zone}\", IdType=\"ImmutableId\""),
        )
        .send()
        .await
        .map_err(|_| "Could not retrieve the original all-day date.")?;
    let value = json(response).await?;
    let date = value["start"]["dateTime"]
        .as_str()
        .ok_or("All-day date is unavailable.")?
        .chars()
        .take(10)
        .collect::<String>();
    chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .map_err(|_| "Invalid all-day calendar date.")?;
    event.all_day = Some(date);
    Ok(())
}
pub async fn missing(
    a: &Account,
    c: &Calendar,
    token: &str,
    id: &str,
) -> Result<Option<Occurrence>, String> {
    let mut url = graph_url("me/events/")?;
    url.path_segments_mut().unwrap().pop_if_empty().push(id);
    let response = client()?
        .get(url)
        .bearer_auth(token)
        .header("Prefer", "outlook.timezone=\"UTC\", IdType=\"ImmutableId\"")
        .send()
        .await
        .map_err(|_| "Could not verify a changed calendar event.")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let value = json(response).await?;
    let mut event = parse(&value, a, c)?;
    fix_all_day(&mut event, &value, token).await?;
    Ok(Some(event))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expanded_occurrences_preserve_identity_and_declines() {
        let a = Account {
            id: "a".into(),
            name: "Example".into(),
            client_id: String::new(),
            refresh: String::new(),
            status: String::new(),
            last_sync: 0,
        };
        let c = Calendar {
            key: "c".into(),
            id: "c".into(),
            account: "a".into(),
            name: "Calendar".into(),
            enabled: true,
        };
        let mut v = serde_json::json!({"id":"immutable","iCalUId":"occurrence-1","subject":"Review","seriesMasterId":"series","start":{"dateTime":"2026-10-13T14:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-10-13T15:00:00Z","timeZone":"UTC"},"responseStatus":{"response":"accepted"}});
        let e = parse(&v, &a, &c).unwrap();
        assert_eq!(e.end - e.start, 3600);
        assert_eq!(e.series, "series");
        assert!(!e.cancelled);
        v["responseStatus"]["response"] = "declined".into();
        assert!(parse(&v, &a, &c).unwrap().cancelled);
        v["iCalUId"] = "occurrence-2".into();
        assert_ne!(parse(&v, &a, &c).unwrap().key, e.key);
        v["start"]["dateTime"] = "invalid".into();
        assert!(parse(&v, &a, &c).is_err());
    }
    #[test]
    fn endpoint_fence_and_client_registration_validation() {
        assert!(graph_url("https://example.com/calendar").is_err());
        assert!(graph_url("https://graph.microsoft.com.evil.com/v1.0/me").is_err());
        assert!(validate_client("secret-not-a-client-id").is_err());
        assert!(validate_client("00000000-0000-0000-0000-000000000001").is_ok());
    }
}
