// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! Property lists: a value from TOML, written as the XML of an Info.plist or as the binary
//! `bplist00` a `.DS_Store` holds, each as Apple's tools and Python's `plistlib` write them.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::io;

use toml::Value;
use toml::value::{Datetime, Offset};

use crate::bad;

/// A property list value; a dict's keys sorted, as `plistlib` sorts them.
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
  Bool(bool),
  Int(i64),
  Real(f64),
  /// Whole seconds since 1970-01-01 UTC.
  Date(i64),
  String(String),
  Array(Vec<Plist>),
  Dict(BTreeMap<String, Plist>),
}

impl From<bool> for Plist {
  fn from(x: bool) -> Self {
    Self::Bool(x)
  }
}

impl From<i64> for Plist {
  fn from(x: i64) -> Self {
    Self::Int(x)
  }
}

impl From<f64> for Plist {
  fn from(x: f64) -> Self {
    Self::Real(x)
  }
}

impl From<&str> for Plist {
  fn from(x: &str) -> Self {
    Self::String(x.into())
  }
}

impl From<String> for Plist {
  fn from(x: String) -> Self {
    Self::String(x)
  }
}

/// The seconds from 1970 to 2001-01-01, a binary plist's epoch.
const APPLE_EPOCH: i64 = 978_307_200;

impl Plist {
  /// A dict of `items`.
  pub fn dict<K: Into<String>>(items: impl IntoIterator<Item = (K, Plist)>) -> Self {
    Self::Dict(items.into_iter().map(|(k, v)| (k.into(), v)).collect())
  }

  /// The plist of a TOML `value`, `what` naming it in the errors. A datetime with an offset is a
  /// date, in UTC and whole seconds; one without (a local datetime, date or time) is no instant, so
  /// it is its TOML text as a string. A float is finite.
  pub fn toml(value: &Value, what: &str) -> io::Result<Self> {
    Ok(match value {
      Value::String(s) => Self::String(s.clone()),
      Value::Integer(n) => Self::Int(*n),
      Value::Float(x) if x.is_finite() => Self::Real(*x),
      Value::Float(x) => return Err(bad(format!("{what} is {x}, which a plist has no real for"))),
      Value::Boolean(b) => Self::Bool(*b),
      Value::Datetime(dt) => instant(dt).map_or_else(|| Self::String(dt.to_string()), Self::Date),
      Value::Array(items) => {
        let items = items.iter().enumerate().map(|(i, x)| Self::toml(x, &format!("{what}[{i}]")));
        Self::Array(items.collect::<io::Result<_>>()?)
      }
      Value::Table(table) => {
        let items =
          table.iter().map(|(k, x)| Ok((k.clone(), Self::toml(x, &format!("{what}.{k}"))?)));
        Self::Dict(items.collect::<io::Result<_>>()?)
      }
    })
  }

  // MARK: XML

  /// The XML plist, tab-indented, as `plutil -convert xml1` writes it. Only `&`, `<` and `>` are
  /// escaped; a control character other than tab, LF and CR, which XML can't hold, is an error.
  pub fn xml(&self) -> io::Result<String> {
    let mut out = String::from(concat!(
      "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
      "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
      "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
      "<plist version=\"1.0\">\n",
    ));

    self.element(&mut out, 0)?;
    out += "</plist>\n";
    Ok(out)
  }

  /// Writes this value at `depth` tabs, with its line end.
  fn element(&self, out: &mut String, depth: usize) -> io::Result<()> {
    let tabs = "\t".repeat(depth);
    match self {
      Self::Bool(b) => *out += &format!("{tabs}<{b}/>\n"),
      Self::Int(n) => *out += &format!("{tabs}<integer>{n}</integer>\n"),
      Self::Real(x) => *out += &format!("{tabs}<real>{x:?}</real>\n"),
      Self::Date(secs) => *out += &format!("{tabs}<date>{}</date>\n", iso8601(*secs)),
      Self::String(s) => *out += &format!("{tabs}<string>{}</string>\n", escape(s)?),
      Self::Array(items) if items.is_empty() => *out += &format!("{tabs}<array/>\n"),
      Self::Array(items) => {
        *out += &format!("{tabs}<array>\n");
        for item in items {
          item.element(out, depth + 1)?;
        }

        *out += &format!("{tabs}</array>\n");
      }
      Self::Dict(items) if items.is_empty() => *out += &format!("{tabs}<dict/>\n"),
      Self::Dict(items) => {
        *out += &format!("{tabs}<dict>\n");
        for (key, value) in items {
          *out += &format!("{tabs}\t<key>{}</key>\n", escape(key)?);
          value.element(out, depth + 1)?;
        }

        *out += &format!("{tabs}</dict>\n");
      }
    }

    Ok(())
  }

  // MARK: Binary

  /// The binary plist (`bplist00`) as `plistlib.dumps(fmt=FMT_BINARY)` writes it: the objects in
  /// the order it walks them (a container, then a dict's keys, then its values), each equal scalar
  /// once, then the offset table and the trailer, refs and offsets in the fewest bytes that hold
  /// them.
  pub fn binary(&self) -> Vec<u8> {
    let mut objects = Objects::default();
    objects.flatten(Node::Value(self));

    let refs = size_of(objects.list.len() as u64);
    let mut out = b"bplist00".to_vec();
    let mut offsets = vec![];
    for object in &objects.list {
      offsets.push(out.len() as u64);
      match object {
        Object::Scalar(x) => encode(x, &mut out),
        Object::Array(items) => {
          length(0xA0, items.len(), &mut out);
          items.iter().for_each(|&i| out.extend_from_slice(&be(i as u64, refs)));
        }
        Object::Dict(keys, values) => {
          length(0xD0, keys.len(), &mut out);
          keys.iter().chain(values).for_each(|&i| out.extend_from_slice(&be(i as u64, refs)));
        }
      }
    }

    let table = out.len() as u64;
    let size = size_of(table);
    offsets.iter().for_each(|&at| out.extend_from_slice(&be(at, size)));
    // Five unused bytes and the sort version, then the sizes, the count, the top object (the
    // first) and the table's offset.
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&[size as u8, refs as u8]);
    for n in [objects.list.len() as u64, 0, table] {
      out.extend_from_slice(&n.to_be_bytes());
    }

    out
  }
}

/// A value a binary plist writes, deduplicated by its type and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Scalar<'a> {
  Bool(bool),
  Int(i64),
  /// A real's bits.
  Real(u64),
  Date(i64),
  String(&'a str),
}

enum Object<'a> {
  Scalar(Scalar<'a>),
  Array(Vec<usize>),
  Dict(Vec<usize>, Vec<usize>),
}

/// A dict's key or a value, as a binary plist walks them.
enum Node<'a> {
  Key(&'a str),
  Value(&'a Plist),
}

/// The objects of a binary plist in order, each scalar's index once.
#[derive(Default)]
struct Objects<'a> {
  list: Vec<Object<'a>>,
  scalars: HashMap<Scalar<'a>, usize>,
}

impl<'a> Objects<'a> {
  /// Adds `node` and what it holds; gives its index.
  fn flatten(&mut self, node: Node<'a>) -> usize {
    let scalar = match node {
      Node::Key(s) => Scalar::String(s),
      Node::Value(Plist::Bool(b)) => Scalar::Bool(*b),
      Node::Value(Plist::Int(n)) => Scalar::Int(*n),
      Node::Value(Plist::Real(x)) => Scalar::Real(x.to_bits()),
      Node::Value(Plist::Date(secs)) => Scalar::Date(*secs),
      Node::Value(Plist::String(s)) => Scalar::String(s),
      Node::Value(Plist::Array(items)) => {
        let at = self.push(Object::Array(vec![]));
        let refs = items.iter().map(|x| self.flatten(Node::Value(x))).collect();
        self.list[at] = Object::Array(refs);
        return at;
      }
      Node::Value(Plist::Dict(items)) => {
        let at = self.push(Object::Dict(vec![], vec![]));
        let keys = items.keys().map(|k| self.flatten(Node::Key(k))).collect();
        let values = items.values().map(|x| self.flatten(Node::Value(x))).collect();
        self.list[at] = Object::Dict(keys, values);
        return at;
      }
    };

    if let Some(&at) = self.scalars.get(&scalar) {
      return at;
    }

    let at = self.push(Object::Scalar(scalar));
    self.scalars.insert(scalar, at);
    at
  }

  fn push(&mut self, object: Object<'a>) -> usize {
    self.list.push(object);
    self.list.len() - 1
  }
}

/// Writes a scalar's object.
fn encode(x: &Scalar, out: &mut Vec<u8>) {
  match *x {
    Scalar::Bool(b) => out.push(if b { 0x09 } else { 0x08 }),
    Scalar::Int(n) if n < 0 => {
      out.push(0x13);
      out.extend_from_slice(&n.to_be_bytes());
    }
    Scalar::Int(n) => {
      let size = size_of(n as u64);
      out.push(0x10 | size.trailing_zeros() as u8);
      out.extend_from_slice(&be(n as u64, size));
    }
    Scalar::Real(bits) => {
      out.push(0x23);
      out.extend_from_slice(&bits.to_be_bytes());
    }
    Scalar::Date(secs) => {
      out.push(0x33);
      out.extend_from_slice(&((secs - APPLE_EPOCH) as f64).to_be_bytes());
    }
    Scalar::String(s) if s.is_ascii() => {
      length(0x50, s.len(), out);
      out.extend_from_slice(s.as_bytes());
    }
    Scalar::String(s) => {
      let units: Vec<u16> = s.encode_utf16().collect();
      length(0x60, units.len(), out);
      units.iter().for_each(|u| out.extend_from_slice(&u.to_be_bytes()));
    }
  }
}

/// A marker of `kind` and its count `n`: in the marker's low nibble below 15, else an integer
/// object after it.
fn length(kind: u8, n: usize, out: &mut Vec<u8>) {
  if n < 15 {
    out.push(kind | n as u8);
    return;
  }

  out.push(kind | 0x0F);
  encode(&Scalar::Int(n as i64), out);
}

/// The bytes, 1, 2, 4 or 8, that hold `n`.
fn size_of(n: u64) -> usize {
  match n {
    0..0x100 => 1,
    0x100..0x1_0000 => 2,
    0x1_0000..0x1_0000_0000 => 4,
    _ => 8,
  }
}

/// `n` in its last `size` big-endian bytes.
fn be(n: u64, size: usize) -> Vec<u8> {
  n.to_be_bytes()[8 - size..].to_vec()
}

// MARK: Text

/// `text` with `&`, `<` and `>` as entities; an error if XML can't hold one of its characters.
fn escape(text: &str) -> io::Result<String> {
  let mut out = String::with_capacity(text.len());
  for c in text.chars() {
    match c {
      '&' => out += "&amp;",
      '<' => out += "&lt;",
      '>' => out += "&gt;",
      '\t' | '\n' | '\r' => out.push(c),
      c if c < ' ' || c == '\u{FFFE}' || c == '\u{FFFF}' => {
        return Err(bad(format!("{text:?}: a plist's XML can't hold {c:?}")));
      }
      c => out.push(c),
    }
  }

  Ok(out)
}

/// The instant of a TOML datetime with a date, a time and an offset, in whole seconds since 1970.
fn instant(dt: &Datetime) -> Option<i64> {
  let (Some(date), Some(time), Some(offset)) = (dt.date, dt.time, dt.offset) else {
    return None;
  };

  let minutes = match offset {
    Offset::Z => 0,
    Offset::Custom { minutes } => minutes as i64,
  };

  let days = days(date.year as i64, date.month as i64, date.day as i64);
  let secs = time.hour as i64 * 3600 + time.minute as i64 * 60 + time.second.unwrap_or(0) as i64;
  Some(days * 86400 + secs - minutes * 60)
}

/// `secs` since 1970 as `YYYY-MM-DDTHH:MM:SSZ`.
fn iso8601(secs: i64) -> String {
  let (days, secs) = (secs.div_euclid(86400), secs.rem_euclid(86400));
  let (y, m, d) = civil(days);
  let mut out = format!("{y:04}-{m:02}-{d:02}");
  let _ = write!(out, "T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60);
  out
}

/// The days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days(y: i64, m: i64, d: i64) -> i64 {
  let y = if m <= 2 { y - 1 } else { y };
  let era = y.div_euclid(400);
  let yoe = y - era * 400;
  let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
  let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  era * 146_097 + doe - 719_468
}

/// The date of `days` since 1970-01-01, [days] reversed (`civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
  let z = days + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z - era * 146_097;
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let m = if mp < 10 { mp + 3 } else { mp - 9 };
  (yoe + era * 400 + (m <= 2) as i64, m, d)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A plist of every kind, and the Python literal `plistlib` reads as the same, its date naive in
  /// UTC as any Python 3 reads it.
  fn sample() -> (Plist, &'static str) {
    let toml: toml::Table = toml::from_str(concat!(
      "s = \"a & <b> é 𝄞\"\nn = 7\nneg = -2\nbig = 70000\nx = 0.5\nt = true\nf = false\n",
      "when = 2026-10-30T12:00:00+02:00\nday = 2026-10-30\nlist = [1, \"a\", 1, []]\n",
      "[sub]\nn = 7\nempty = {}\n",
    ))
    .unwrap();

    let plist = Plist::toml(&Value::Table(toml), "sample").unwrap();
    let py = concat!(
      "{'s': 'a & <b> é 𝄞', 'n': 7, 'neg': -2, 'big': 70000, 'x': 0.5, 't': True, 'f': False,",
      " 'when': datetime.datetime(2026, 10, 30, 10, 0), 'day': '2026-10-30',",
      " 'list': [1, 'a', 1, []], 'sub': {'n': 7, 'empty': {}}}",
    );

    (plist, py)
  }

  #[test]
  fn from_toml() {
    let (plist, _) = sample();
    let Plist::Dict(d) = &plist else { panic!("{plist:?}") };
    assert_eq!(d["when"], Plist::Date(1_793_354_400));
    assert_eq!(d["day"], Plist::from("2026-10-30"));
    assert_eq!(d["list"], Plist::Array(vec![1.into(), "a".into(), 1.into(), Plist::Array(vec![])]));

    let inf = Value::Float(f64::INFINITY);
    assert_eq!(
      Plist::toml(&inf, "k").unwrap_err().to_string(),
      "k is inf, which a plist has no real for"
    );
    let nested: toml::Table = toml::from_str("a = [{ b = nan }]").unwrap();
    let e = Plist::toml(&Value::Table(nested), "p").unwrap_err().to_string();
    assert_eq!(e, "p.a[0].b is NaN, which a plist has no real for");
  }

  #[test]
  fn dates() {
    for (secs, text) in [
      (0, "1970-01-01T00:00:00Z"),
      (951_782_400, "2000-02-29T00:00:00Z"),
      (-1, "1969-12-31T23:59:59Z"),
      (1_793_354_400, "2026-10-30T10:00:00Z"),
    ] {
      assert_eq!(iso8601(secs), text);
    }

    let dt: Datetime = "2026-01-01T00:30:00-01:00".parse().unwrap();
    assert_eq!(instant(&dt).map(iso8601).as_deref(), Some("2026-01-01T01:30:00Z"));
    let dt: Datetime = "2026-01-01T00:30:00".parse().unwrap();
    assert_eq!(instant(&dt), None);
  }

  #[test]
  fn xml() {
    let (plist, _) = sample();
    let want = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>big</key>
\t<integer>70000</integer>
\t<key>day</key>
\t<string>2026-10-30</string>
\t<key>f</key>
\t<false/>
\t<key>list</key>
\t<array>
\t\t<integer>1</integer>
\t\t<string>a</string>
\t\t<integer>1</integer>
\t\t<array/>
\t</array>
\t<key>n</key>
\t<integer>7</integer>
\t<key>neg</key>
\t<integer>-2</integer>
\t<key>s</key>
\t<string>a &amp; &lt;b&gt; é 𝄞</string>
\t<key>sub</key>
\t<dict>
\t\t<key>empty</key>
\t\t<dict/>
\t\t<key>n</key>
\t\t<integer>7</integer>
\t</dict>
\t<key>t</key>
\t<true/>
\t<key>when</key>
\t<date>2026-10-30T10:00:00Z</date>
\t<key>x</key>
\t<real>0.5</real>
</dict>
</plist>
";
    assert_eq!(plist.xml().unwrap(), want);

    let e = Plist::dict([("k", Plist::from("a\u{1}"))]).xml().unwrap_err().to_string();
    assert_eq!(e, "\"a\\u{1}\": a plist's XML can't hold '\\u{1}'");
  }

  #[test]
  fn binary_sizes() {
    let mut out = vec![];
    for (n, want) in [(0, &[0x10, 0][..]), (255, &[0x10, 255]), (256, &[0x11, 1, 0])] {
      out.clear();
      encode(&Scalar::Int(n), &mut out);
      assert_eq!(out, want);
    }

    out.clear();
    encode(&Scalar::Int(1 << 40), &mut out);
    assert_eq!(out, [0x13, 0, 0, 1, 0, 0, 0, 0, 0]);
    out.clear();
    length(0x50, 15, &mut out);
    assert_eq!(out, [0x5F, 0x10, 15]);

    // More than 255 objects take two-byte refs.
    let many = Plist::Array((0..300).map(Plist::Int).collect());
    let data = many.binary();
    assert_eq!(&data[data.len() - 26..data.len() - 24], [2, 2]);
  }

  /// Runs `python3` with `code`, `data` on its stdin; its stdout.
  #[cfg(target_os = "macos")]
  fn python(code: &str, data: &[u8]) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("python3");
    cmd.args(["-I", "-c", code]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(data).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
  }

  /// Both forms read back by Python's `plistlib` as the value written, the binary one byte for
  /// byte what `plistlib` writes, and both accepted by `plutil -lint`.
  #[cfg(target_os = "macos")]
  #[test]
  fn read_back() {
    use std::process::Command;

    use crate::tool::{Temp, run};

    let (plist, py) = sample();
    let code = |load: &str| {
      format!("import sys, plistlib, datetime\nv = {py}\nd = sys.stdin.buffer.read()\n{load}")
    };

    let same = "print(plistlib.loads(d) == v)";
    assert_eq!(python(&code(same), plist.xml().unwrap().as_bytes()), "True\n");
    assert_eq!(python(&code(same), &plist.binary()), "True\n");
    let dump = "print(plistlib.dumps(v, fmt=plistlib.FMT_BINARY) == d)";
    assert_eq!(python(&code(dump), &plist.binary()), "True\n");

    let tmp = Temp::new().unwrap();
    for (name, data) in
      [("a.plist", plist.xml().unwrap().into_bytes()), ("b.plist", plist.binary())]
    {
      std::fs::write(tmp.join(name), data).unwrap();
      run(Command::new("plutil").arg("-lint").arg(tmp.join(name))).unwrap();
    }
  }
}
