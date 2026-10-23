//! Windows toast reminders for tasks that have a time.

use crate::{desktop::APP_ID, store::Store};
use chrono::{Local, NaiveDateTime};
use std::{thread, time::Duration};
use tauri::{AppHandle, Manager};
use tauri_winrt_notification::{Sound, Toast};

/// A reminder is still shown if the app notices the moment this late
/// (e.g. after waking from sleep); older ones just stay overdue in the widget.
const GRACE_MINUTES: i64 = 15;

pub fn spawn(app: AppHandle) {
    thread::spawn(move || loop {
        check(&app);
        thread::sleep(Duration::from_secs(15));
    });
}

fn check(app: &AppHandle) {
    let store = app.state::<Store>();
    let now = Local::now().naive_local();
    let mut due_now = Vec::new();
    {
        let mut tasks = store.tasks.lock().unwrap();
        for t in tasks.iter_mut().filter(|t| !t.done) {
            let (Some(date), Some(time)) = (&t.due, &t.time) else { continue };
            let key = format!("{date} {time}");
            if t.notified.as_deref() == Some(key.as_str()) {
                continue;
            }
            let Ok(at) = NaiveDateTime::parse_from_str(&key, "%Y-%m-%d %H:%M") else { continue };
            let late = now.signed_duration_since(at).num_minutes();
            if now >= at && late <= GRACE_MINUTES {
                due_now.push((t.title.clone(), time.clone()));
                t.notified = Some(key);
            }
        }
        if !due_now.is_empty() {
            // `notified` stays in memory even if the write fails, so a reminder
            // is never repeated every 15 s.
            let _ = store.save(&tasks);
        }
    }
    for (title, time) in due_now {
        let _ = Toast::new(APP_ID)
            .title(&title)
            .text1(&format!("Пора · {time}"))
            .sound(Some(Sound::Reminder))
            .duration(tauri_winrt_notification::Duration::Long)
            .show();
    }
}
