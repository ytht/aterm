//! 简繁逐字映射（OpenCC STCharacters / TSCharacters 单字表，取每行首选）。
//! 用于输入内码转换：GBK→Big5 时简→繁、Big5→GBK 时繁→简，
//! 避免目标字符集缺字时 encoding_rs 输出 HTML 数字引用（如 &#21326;）。

use std::collections::HashMap;
use std::sync::OnceLock;

fn build(table: &str) -> HashMap<char, char> {
    let mut m = HashMap::new();
    for line in table.lines() {
        let mut it = line.split_whitespace();
        if let (Some(a), Some(b)) = (it.next(), it.next()) {
            let mut ca = a.chars();
            let mut cb = b.chars();
            if let (Some(x), Some(y), None, None) = (ca.next(), cb.next(), ca.next(), cb.next()) {
                m.entry(x).or_insert(y);
            }
        }
    }
    m
}

static S2T: OnceLock<HashMap<char, char>> = OnceLock::new();
static T2S: OnceLock<HashMap<char, char>> = OnceLock::new();

/// 简→繁（无映射原样返回）。
pub fn s2t(c: char) -> char {
    let m = S2T.get_or_init(|| build(include_str!("s2t.txt")));
    m.get(&c).copied().unwrap_or(c)
}

/// 繁→简（无映射原样返回）。
pub fn t2s(c: char) -> char {
    let m = T2S.get_or_init(|| build(include_str!("t2s.txt")));
    m.get(&c).copied().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    #[test]
    fn zh_map_basic() {
        assert_eq!(super::s2t('\u{534E}'), '\u{83EF}'); // 华→華
        assert_eq!(super::s2t('\u{590F}'), '\u{590F}'); // 夏不变
        assert_eq!(super::t2s('\u{83EF}'), '\u{534E}'); // 華→华
        assert_eq!(super::t2s('\u{590F}'), '\u{590F}');
    }
}
