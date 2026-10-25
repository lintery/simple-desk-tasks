//! Plain HTTPS GET for the updater and the focus sounds (WinRT HttpClient,
//! which follows GitHub's download redirects).

use crate::desktop::finish;
use std::time::Duration;
use windows::{core::HSTRING, Foundation::Uri, Storage::Streams::DataReader, Web::Http::HttpClient};

/// The body of `url`; a non-2xx answer is an error. Call from a thread with
/// WinRT initialized.
pub fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let fetch = || -> windows::core::Result<Vec<u8>> {
        let client = HttpClient::new()?;
        let headers = client.DefaultRequestHeaders()?;
        headers.UserAgent()?.TryParseAdd(&HSTRING::from("SimpleDeskTasks"))?;
        headers.TryAppendWithoutValidation(&HSTRING::from("Accept"), &HSTRING::from("application/vnd.github+json, */*"))?;
        let buffer = finish(client.GetBufferAsync(&Uri::CreateUri(&HSTRING::from(url))?)?, timeout)?;
        let mut bytes = vec![0u8; buffer.Length()? as usize];
        DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
        Ok(bytes)
    };
    fetch().map_err(|e| e.message().to_string())
}
