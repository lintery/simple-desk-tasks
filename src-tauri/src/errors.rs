//! Errors as the user reads them: what went wrong and what to do about it, in
//! plain Russian — never an OS error code, an HTTP status, an HRESULT or an
//! English library message.

use std::io;

/// A file or folder operation (reading or writing) that failed; `what` says
/// what didn't happen («Задачи не сохранены», «Файл задач не открывается»).
pub fn file(e: &io::Error, what: &str) -> String {
    let why = match e.raw_os_error() {
        Some(2 | 3 | 53 | 67) => "файл или папка не найдены",
        Some(5) => "нет доступа — его может не давать антивирус или права на папку",
        Some(19) => "диск защищён от записи",
        Some(21 | 1167) => "диск не подключён",
        Some(32 | 33) => "файл занят другой программой",
        Some(39 | 112) => "на диске не осталось места",
        _ => match e.kind() {
            io::ErrorKind::NotFound => "файл или папка не найдены",
            io::ErrorKind::PermissionDenied => "нет доступа",
            _ => "Windows не дала открыть или записать файл",
        },
    };
    format!("{what}: {why}")
}

/// An HTTP answer other than success from `site` («calendar.mail.ru», «GitHub»).
pub fn http(status: u16, site: &str) -> String {
    match status {
        401 => format!("{site} не принял логин или пароль"),
        // GitHub says 403 when it limits requests; a calendar handles it itself.
        403 => format!("{site} не пускает — попробую позже"),
        404 | 410 => format!("{site}: адрес не найден — возможно, он изменился"),
        408 | 429 => format!("{site} просит подождать — попробую позже"),
        500..=599 => format!("{site} сейчас не работает — попробую позже"),
        _ => format!("{site} не принял запрос"),
    }
}

/// A network request to `site` that failed before (or instead of) an answer.
pub fn net(e: &windows::core::Error, site: &str) -> String {
    let code = e.code().0 as u32;
    match code {
        // The request took too long (see desktop::finish) or WinINet gave up.
        0x8007_05B4 | 0x8007_2EE2 => format!("{site} не ответил вовремя — проверьте интернет"),
        // Name not resolved, can't connect, connection dropped or reset.
        0x8007_2EE7 | 0x8007_2EFD | 0x8007_2EFE | 0x8007_2EFF | 0x8007_2F78 => {
            format!("Нет связи с {site} — проверьте интернет")
        }
        // Certificate problems: most often the computer's clock is off.
        0x8007_2F05..=0x8007_2F0D | 0x8007_2F17 | 0x8007_2F8F => {
            format!("Не удаётся защищённо соединиться с {site} — проверьте дату и время на компьютере")
        }
        // HTTP_E_STATUS_*: an answer other than success, the status in the low bits.
        0x8019_0000..=0x8019_FFFF => http((code & 0xFFFF) as u16, site),
        _ => format!("Нет связи с {site} — проверьте интернет"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::{Error, HRESULT};

    #[test]
    fn file_errors_say_what_happened() {
        let e = io::Error::from_raw_os_error(112);
        assert_eq!(file(&e, "Задачи не сохранены"), "Задачи не сохранены: на диске не осталось места");
        assert!(!file(&io::Error::from_raw_os_error(5), "x").contains("error"));
    }

    #[test]
    fn network_errors_have_no_codes() {
        let dns = Error::from_hresult(HRESULT(0x8007_2EE7_u32 as i32));
        assert_eq!(net(&dns, "GitHub"), "Нет связи с GitHub — проверьте интернет");
        let forbidden = Error::from_hresult(HRESULT(0x8019_0193_u32 as i32));
        assert_eq!(net(&forbidden, "GitHub"), "GitHub не пускает — попробую позже");
        assert_eq!(http(503, "calendar.mail.ru"), "calendar.mail.ru сейчас не работает — попробую позже");
    }
}
