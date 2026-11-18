use crate::{Result, StoreError};
use axum::{
  body::Body,
  extract::Request,
  http::{header, HeaderMap, HeaderValue, Method, StatusCode},
  response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::io::SeekFrom;
use tokio::{
  io::{AsyncReadExt, AsyncSeekExt},
  sync::OwnedSemaphorePermit,
};
use tokio_util::io::ReaderStream;

pub(crate) async fn file_response(
  file: std::fs::File,
  size: u64,
  hash: &str,
  content_type: &'static str,
  filename: Option<&str>,
  permit: OwnedSemaphorePermit,
  request: Request,
) -> Result<Response> {
  let etag = format!("\"{hash}\"");
  let request_headers = request.headers();
  let not_modified = request_headers
    .get(header::IF_NONE_MATCH)
    .and_then(|value| value.to_str().ok())
    == Some(etag.as_str());
  let package = filename.is_some();
  let range = if package && request.method() == Method::GET && !not_modified {
    requested_range(request_headers, size, &etag)
  } else {
    Ok(None)
  };
  let (mut response, length, content_range) = match range {
    _ if not_modified => (StatusCode::NOT_MODIFIED.into_response(), None, None),
    Err(()) => (
      StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
      Some(0),
      Some(format!("bytes */{size}")),
    ),
    Ok(Some((start, end))) => {
      let length = end - start + 1;
      let mut file = tokio::fs::File::from_std(file);
      file
        .seek(SeekFrom::Start(start))
        .await
        .map_err(|_| StoreError::internal())?;
      (
        (StatusCode::PARTIAL_CONTENT, file_body(file, length, permit)).into_response(),
        Some(length),
        Some(format!("bytes {start}-{end}/{size}")),
      )
    }
    Ok(None) if request.method() == Method::HEAD => {
      (Response::new(Body::empty()), Some(size), None)
    }
    Ok(None) => (
      Response::new(file_body(tokio::fs::File::from_std(file), size, permit)),
      Some(size),
      None,
    ),
  };
  let unsatisfiable = response.status() == StatusCode::RANGE_NOT_SATISFIABLE;
  let headers = response.headers_mut();
  headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
  headers.insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static(if unsatisfiable {
      "no-store"
    } else if package {
      "public, max-age=31536000, immutable, no-transform"
    } else {
      "public, max-age=31536000, immutable"
    }),
  );
  headers.insert(
    header::ETAG,
    HeaderValue::from_str(&etag).map_err(|_| StoreError::internal())?,
  );
  if let Some(length) = length {
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
  }
  if let Some(content_range) = content_range {
    headers.insert(
      header::CONTENT_RANGE,
      HeaderValue::from_str(&content_range).map_err(|_| StoreError::internal())?,
    );
  }
  if let Some(filename) = filename {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
      header::CONTENT_DISPOSITION,
      HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|_| StoreError::internal())?,
    );
  }
  Ok(response)
}

fn file_body(file: tokio::fs::File, length: u64, permit: OwnedSemaphorePermit) -> Body {
  let stream = ReaderStream::with_capacity(file.take(length), 64 * 1024).map(move |chunk| {
    let _keep_permit_until_body_is_dropped = &permit;
    chunk
  });
  Body::from_stream(stream)
}

/// Unsupported/malformed ranges are ignored. A valid single range outside
/// the file returns an error so the caller emits 416 with its current length.
fn requested_range(
  headers: &HeaderMap,
  size: u64,
  etag: &str,
) -> std::result::Result<Option<(u64, u64)>, ()> {
  let mut validators = headers.get_all(header::IF_RANGE).iter();
  if let Some(validator) = validators.next() {
    // The content hash is our only strong validator. Do not accept dates or
    // weak ETags: the caller must replace its local partial file in that case.
    if validator.as_bytes() != etag.as_bytes() || validators.next().is_some() {
      return Ok(None);
    }
  }
  let mut values = headers.get_all(header::RANGE).iter();
  let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
    return Ok(None);
  };
  let Some((unit, range)) = value.trim().split_once('=') else {
    return Ok(None);
  };
  if !unit.eq_ignore_ascii_case("bytes") || values.next().is_some() || range.contains(',') {
    return Ok(None);
  }
  let Some((start, end)) = range.split_once('-') else {
    return Ok(None);
  };
  if start.is_empty() {
    let Some(suffix) = decimal(end) else {
      return Ok(None);
    };
    if suffix == 0 || size == 0 {
      return Err(());
    }
    return Ok(Some((size.saturating_sub(suffix), size - 1)));
  }
  let Some(start) = decimal(start) else {
    return Ok(None);
  };
  let end = if end.is_empty() {
    u64::MAX
  } else {
    let Some(end) = decimal(end) else {
      return Ok(None);
    };
    end
  };
  if start > end {
    return Ok(None);
  }
  if start >= size {
    return Err(());
  }
  Ok(Some((start, end.min(size - 1))))
}

fn decimal(value: &str) -> Option<u64> {
  if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  // Arbitrarily large decimal inputs are valid syntax. Sizes are far smaller
  // than u64::MAX, so that sentinel safely represents numbers beyond u64.
  Some(value.parse().unwrap_or(u64::MAX))
}
