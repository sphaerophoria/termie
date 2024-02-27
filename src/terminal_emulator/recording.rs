use crate::error::backtraced_err;

use std::{
    collections::HashMap,
    num::TryFromIntError,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use thiserror::Error;
use tinyjson::JsonValue;

#[derive(Debug, Error)]
#[error("not a map")]
pub struct NotMap;

#[derive(Debug, Error)]
#[error("not an array")]
pub struct NotArray;

#[derive(Debug, Error)]
#[error("not a string")]
pub struct NotString;

#[derive(Debug, Error)]
#[error("not a bool")]
pub struct NotBool;

#[derive(Debug, Error)]
enum NotIntOfTypeKind {
    #[error("not a number")]
    NotNumber,
    #[error("number does not cast to type")]
    Cast(#[from] std::num::TryFromIntError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct NotIntOfType(#[from] NotIntOfTypeKind);

#[derive(Debug)]
pub enum SnapshotItem {
    Bool(bool),
    Int(i64),
    String(String),
    Array(Vec<SnapshotItem>),
    Map(HashMap<String, SnapshotItem>),
}

impl SnapshotItem {
    pub fn into_map(self) -> Result<HashMap<String, SnapshotItem>, NotMap> {
        match self {
            SnapshotItem::Map(map) => Ok(map),
            _ => Err(NotMap),
        }
    }

    pub fn into_vec(self) -> Result<Vec<SnapshotItem>, NotArray> {
        match self {
            SnapshotItem::Array(v) => Ok(v),
            _ => Err(NotArray),
        }
    }

    pub fn into_bool(self) -> Result<bool, NotBool> {
        match self {
            SnapshotItem::Bool(v) => Ok(v),
            _ => Err(NotBool),
        }
    }

    pub fn into_i64(self) -> Result<i64, NotIntOfType> {
        match self {
            SnapshotItem::Int(v) => Ok(v),
            _ => Err(NotIntOfTypeKind::NotNumber)?,
        }
    }

    pub fn into_num<T: TryFrom<i64, Error = std::num::TryFromIntError>>(
        self,
    ) -> Result<T, NotIntOfType> {
        let v = self.into_i64()?;
        Ok(v.try_into().map_err(NotIntOfTypeKind::Cast)?)
    }

    pub fn into_string(self) -> Result<String, NotString> {
        match self {
            SnapshotItem::String(s) => Ok(s),
            _ => Err(NotString),
        }
    }
}

macro_rules! impl_from_int {
    ($t:ty) => {
        impl From<$t> for SnapshotItem {
            fn from(value: $t) -> Self {
                SnapshotItem::Int(value.into())
            }
        }
    };
}

macro_rules! impl_from_int_ref {
    ($t:ty) => {
        impl From<&$t> for SnapshotItem {
            fn from(value: &$t) -> Self {
                SnapshotItem::Int((*value).into())
            }
        }
    };
}

macro_rules! impl_from_str {
    ($t:ty) => {
        impl From<$t> for SnapshotItem {
            fn from(value: $t) -> Self {
                SnapshotItem::String(value.into())
            }
        }
    };
}

impl_from_int!(u8);
impl_from_int_ref!(u8);
impl_from_int!(i64);
impl_from_int_ref!(i64);
impl_from_str!(&str);
impl_from_str!(String);

impl From<bool> for SnapshotItem {
    fn from(value: bool) -> Self {
        SnapshotItem::Bool(value)
    }
}

impl<T: Into<SnapshotItem>> FromIterator<T> for SnapshotItem {
    fn from_iter<U: IntoIterator<Item = T>>(iter: U) -> Self {
        let it = iter.into_iter();
        let v: Vec<SnapshotItem> = it.map(|x| x.into()).collect();
        SnapshotItem::Array(v)
    }
}

fn find_recording_path(recording_dir: &Path) -> PathBuf {
    let mut i = 0;
    loop {
        let candidate_path = recording_dir.join(format!("{}.json", i));
        if candidate_path.exists() {
            i += 1;
            continue;
        }
        return candidate_path;
    }
}

fn tinyjson_to_snapshot(value: tinyjson::JsonValue) -> SnapshotItem {
    match value {
        tinyjson::JsonValue::Null => {
            unimplemented!();
        }
        tinyjson::JsonValue::Boolean(b) => SnapshotItem::Bool(b),
        tinyjson::JsonValue::Number(num) => {
            if num.fract() == 0.0 {
                SnapshotItem::Int(num as i64)
            } else {
                unimplemented!();
            }
        }
        tinyjson::JsonValue::String(s) => SnapshotItem::String(s),
        tinyjson::JsonValue::Array(arr) => {
            let v = arr.into_iter().map(tinyjson_to_snapshot).collect();
            SnapshotItem::Array(v)
        }
        tinyjson::JsonValue::Object(map) => {
            let m = map
                .into_iter()
                .map(|(k, v)| (k, tinyjson_to_snapshot(v)))
                .collect();
            SnapshotItem::Map(m)
        }
    }
}

fn snapshot_to_tinyjson(value: SnapshotItem) -> JsonValue {
    match value {
        SnapshotItem::Int(v) => JsonValue::Number(v as f64),
        SnapshotItem::Bool(v) => JsonValue::Boolean(v),
        SnapshotItem::String(v) => JsonValue::String(v),
        SnapshotItem::Array(v) => {
            JsonValue::Array(v.into_iter().map(snapshot_to_tinyjson).collect())
        }
        SnapshotItem::Map(v) => JsonValue::Object(
            v.into_iter()
                .map(|(k, v)| (k, snapshot_to_tinyjson(v)))
                .collect(),
        ),
    }
}

#[derive(Debug, Error)]
enum LoadRecordingErrorKind {
    #[error("failed to read recording")]
    Read(#[source] std::io::Error),
    #[error("failed to parse recording as json")]
    Parse(#[source] tinyjson::JsonParseError),
    #[error("root item is not an object")]
    RootNotObject,
    #[error("initial state field not present")]
    InitialStateMissing,
    #[error("initial state is not an object")]
    InitialStateNotObject,
    #[error("items field not present")]
    ItemsNotPresent,
    #[error("items field is not an array")]
    ItemsNotArray,
    #[error("invalid item in items")]
    ItemInvalid(#[source] ParseRecordingItemError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct LoadRecordingError(#[from] LoadRecordingErrorKind);

#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    initial_state: HashMap<String, tinyjson::JsonValue>,
    items: Vec<RecordingItem>,
}

impl Recording {
    fn new() -> Recording {
        Recording {
            initial_state: Default::default(),
            items: Default::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Recording, LoadRecordingError> {
        use LoadRecordingErrorKind::*;
        let content = std::fs::read_to_string(path).map_err(Read)?;
        let json: tinyjson::JsonValue = content.parse().map_err(Parse)?;
        let tinyjson::JsonValue::Object(mut root) = json else {
            Err(RootNotObject)?
        };

        // FIXME: strings should be constnants
        let initial_state = root.remove("initial_state").ok_or(InitialStateMissing)?;
        let tinyjson::JsonValue::Object(initial_state) = initial_state else {
            Err(InitialStateNotObject)?
        };

        // FIXME: strings should be constnants
        let items = root.remove("items").ok_or(ItemsNotPresent)?;
        let tinyjson::JsonValue::Array(items) = items else {
            Err(ItemsNotArray)?
        };

        let items = items
            .into_iter()
            .map(|v| RecordingItem::from_json(v).map_err(ItemInvalid))
            .collect::<Result<_, _>>()?;

        Ok(Recording {
            initial_state,
            items,
        })
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::Object(
            [
                (
                    "initial_state".to_string(),
                    JsonValue::Object(self.initial_state.clone()),
                ),
                (
                    "items".to_string(),
                    JsonValue::Array(self.items.iter().map(|v| v.to_json()).collect()),
                ),
            ]
            .into(),
        )
    }

    pub fn initial_state(&self) -> SnapshotItem {
        let state: HashMap<String, SnapshotItem> = self
            .initial_state
            .iter()
            .map(|(k, v)| (k.clone(), tinyjson_to_snapshot(v.clone())))
            .collect();

        SnapshotItem::Map(state)
    }

    pub fn items(&self) -> &[RecordingItem] {
        &self.items
    }
}

struct RecordingHandleInner {
    recording: Recording,
    path: PathBuf,
}

impl Drop for RecordingHandleInner {
    fn drop(&mut self) {
        let res = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&self.path)?;

            self.recording.to_json().format_to(&mut f)?;
            Ok(())
        })();

        if let Err(e) = res {
            error!("Failed to save recording: {}", backtraced_err(&*e));
        }
    }
}

#[derive(Error, Debug)]
enum ParseRecordingItemErrorKind {
    #[error("root element is not an object")]
    RootNotObject,
    #[error("type field not present")]
    TypeNotPresent,
    #[error("type field is not a string")]
    TypeNotString,
    #[error("width field not present")]
    WidthNotPresent,
    #[error("width field is not a number")]
    WidthNotNumber,
    #[error("height field is not present")]
    HeightNotPresent,
    #[error("height field is not a number")]
    HeightNotNumber,
    #[error("width field is not a usize")]
    WidthNotUsize(#[source] TryFromIntError),
    #[error("height field is not a usize")]
    HeightNotUsize(#[source] TryFromIntError),
    #[error("data field is not present")]
    DataNotPresent,
    #[error("data field is not an array")]
    DataNotArray,
    #[error("data elem is not a number")]
    DataElemNotNumber,
    #[error("data elem does not fit in u8")]
    DataElemNotU8,
    #[error("unexpected field: {0}")]
    UnexpectedField(String),
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct ParseRecordingItemError(#[from] ParseRecordingItemErrorKind);

#[derive(Clone)]
pub struct RecordingHandle {
    #[allow(unused)]
    inner: Arc<Mutex<RecordingHandleInner>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordingItem {
    SetWinSize { width: usize, height: usize },
    Write { data: Vec<u8> },
}

impl RecordingItem {
    fn from_json(json: JsonValue) -> Result<RecordingItem, ParseRecordingItemError> {
        use ParseRecordingItemErrorKind::*;

        let JsonValue::Object(mut map) = json else {
            Err(RootNotObject)?
        };

        let typ = map.remove("type").ok_or(TypeNotPresent)?;
        let JsonValue::String(typ) = typ else {
            Err(TypeNotString)?
        };

        match typ.as_str() {
            "set_win_size" => {
                let width = map.remove("width").ok_or(WidthNotPresent)?;
                let JsonValue::Number(width) = width else {
                    Err(WidthNotNumber)?
                };

                let height = map.remove("height").ok_or(HeightNotPresent)?;
                let JsonValue::Number(height) = height else {
                    Err(HeightNotNumber)?
                };

                let width = width.round() as i64;
                let width = width.try_into().map_err(WidthNotUsize)?;
                let height = height.round() as i64;
                let height = height.try_into().map_err(HeightNotUsize)?;

                Ok(RecordingItem::SetWinSize { width, height })
            }
            "write" => {
                let data = map.remove("data").ok_or(DataNotPresent)?;
                let JsonValue::Array(data) = data else {
                    Err(DataNotArray)?
                };

                let data: Vec<_> = data
                    .into_iter()
                    .map(|v| -> Result<u8, ParseRecordingItemErrorKind> {
                        let v_num: f64 = *v.get().ok_or(DataElemNotNumber)?;
                        if v_num > u8::MAX as f64 || v_num < u8::MIN as f64 {
                            Err(DataElemNotU8)?
                        }
                        Ok(v_num as u8)
                    })
                    .collect::<Result<_, _>>()?;

                Ok(RecordingItem::Write { data })
            }
            _ => Err(UnexpectedField(typ))?,
        }
    }

    fn to_json(&self) -> tinyjson::JsonValue {
        match self {
            RecordingItem::SetWinSize { width, height } => JsonValue::Object(
                [
                    ("type".into(), JsonValue::String("set_win_size".into())),
                    ("width".into(), JsonValue::Number(*width as f64)),
                    ("height".into(), JsonValue::Number(*height as f64)),
                ]
                .into(),
            ),
            RecordingItem::Write { data } => JsonValue::Object(
                [
                    ("type".into(), JsonValue::String("write".into())),
                    (
                        "data".into(),
                        JsonValue::Array(
                            data.iter().map(|v| JsonValue::Number(*v as f64)).collect(),
                        ),
                    ),
                ]
                .into(),
            ),
        }
    }
}

pub struct RecordingInitializer {
    inner: Arc<Mutex<RecordingHandleInner>>,
}

impl RecordingInitializer {
    pub fn snapshot_item(&self, name: String, item: SnapshotItem) {
        let mut inner = self.inner.lock().expect("poisoned lock");
        let json_value = snapshot_to_tinyjson(item);
        inner.recording.initial_state.insert(name, json_value);
    }

    pub fn into_handle(self) -> RecordingHandle {
        RecordingHandle { inner: self.inner }
    }
}

pub enum StartRecordingResponse {
    New(RecordingInitializer),
    Existing(RecordingHandle),
}

pub struct Recorder {
    recording_dir: PathBuf,
    handle: Weak<Mutex<RecordingHandleInner>>,
}

impl Recorder {
    pub fn new(recording_dir: PathBuf) -> Recorder {
        Recorder {
            recording_dir,
            handle: Weak::new(),
        }
    }

    pub fn set_win_size(&self, width: usize, height: usize) {
        if let Some(inner) = self.handle.upgrade() {
            let mut inner = inner.lock().expect("poisoned lock");
            inner
                .recording
                .items
                .push(RecordingItem::SetWinSize { width, height });
        }
    }

    pub fn write(&self, to_insert: &[u8]) {
        if let Some(inner) = self.handle.upgrade() {
            let mut inner = inner.lock().expect("poisoned lock");
            if let Some(RecordingItem::Write { data }) = inner.recording.items.last_mut() {
                data.extend_from_slice(to_insert);
            } else {
                inner.recording.items.push(RecordingItem::Write {
                    data: to_insert.to_vec(),
                });
            }
        }
    }

    pub fn start_recording(&mut self) -> Result<StartRecordingResponse, std::io::Error> {
        std::fs::create_dir_all(&self.recording_dir)?;

        if let Some(handle) = self.handle.upgrade() {
            return Ok(StartRecordingResponse::Existing(RecordingHandle {
                inner: handle,
            }));
        }

        let recording_path = find_recording_path(&self.recording_dir);

        info!("Recording to {}", recording_path.display());

        let handle_inner = Arc::new(Mutex::new(RecordingHandleInner {
            recording: Recording::new(),
            path: recording_path,
        }));
        self.handle = Arc::downgrade(&handle_inner);

        Ok(StartRecordingResponse::New(RecordingInitializer {
            inner: handle_inner,
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_recorder() {
        let _temp_dir = tempfile::TempDir::new().expect("failed to create tmp dir");
        let mut recorder = Recorder::new(_temp_dir.path().into());

        let StartRecordingResponse::New(initializer) = recorder
            .start_recording()
            .expect("failed to start recording")
        else {
            panic!("Did not get initializer");
        };

        initializer.snapshot_item(
            "test_arr".to_string(),
            SnapshotItem::Array(vec![1u8.into(), 2u8.into(), 3u8.into(), 4u8.into()]),
        );
        initializer.snapshot_item(
            "test_map".to_string(),
            SnapshotItem::Map(
                [
                    ("int".to_string(), 1i64.into()),
                    ("string".to_string(), "hello".into()),
                    ("bool".to_string(), true.into()),
                ]
                .into(),
            ),
        );

        let handle = initializer.into_handle();

        recorder.write(b"asdf");
        recorder.write(b"1234");
        recorder.set_win_size(10, 20);
        recorder.write(b"xyzw");
        let saved = handle
            .inner
            .lock()
            .expect("poisoned lock")
            .recording
            .clone();
        drop(handle);

        let loaded =
            Recording::load(&_temp_dir.path().join("0.json")).expect("failed to load recording");

        assert_eq!(loaded, saved);
    }
}
