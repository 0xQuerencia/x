//! Small, bounded arithmetic evaluator used by X Quick.
//! No shell, JavaScript runtime, network call or language model required.

const MAX_LEN: usize = 128;
const MAX_DEPTH: u8 = 24;

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while self.bytes.get(self.index).is_some_and(u8::is_ascii_whitespace) {
            self.index += 1;
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        self.skip_space();
        if self.bytes.get(self.index) == Some(&byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn number(&mut self) -> Option<f64> {
        self.skip_space();
        let start = self.index;
        let mut digits = 0;
        while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
            self.index += 1;
            digits += 1;
        }
        if self.bytes.get(self.index) == Some(&b'.') {
            self.index += 1;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return None;
        }
        if self.bytes.get(self.index).is_some_and(|x| *x == b'e' || *x == b'E') {
            self.index += 1;
            if self.bytes.get(self.index).is_some_and(|x| *x == b'+' || *x == b'-') {
                self.index += 1;
            }
            let exponent_start = self.index;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
            }
            if self.index == exponent_start {
                return None;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.index]).ok()?;
        let value = text.parse::<f64>().ok()?;
        value.is_finite().then_some(value)
    }

    fn expr(&mut self, minimum: u8, depth: u8) -> Option<f64> {
        if depth >= MAX_DEPTH {
            return None;
        }
        self.skip_space();
        let mut left = if self.take(b'+') {
            self.expr(30, depth + 1)?
        } else if self.take(b'-') {
            -self.expr(30, depth + 1)?
        } else if self.take(b'(') {
            let value = self.expr(0, depth + 1)?;
            self.take(b')').then_some(value)?
        } else {
            self.number()?
        };
        loop {
            self.skip_space();
            let op = *self.bytes.get(self.index).unwrap_or(&0);
            let (precedence, right_associative) = match op {
                b'+' | b'-' => (10, false),
                b'*' | b'/' | b'%' => (20, false),
                b'^' => (40, true),
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            self.index += 1;
            let right = self.expr(precedence + if right_associative { 0 } else { 1 }, depth + 1)?;
            left = match op {
                b'+' => left + right,
                b'-' => left - right,
                b'*' => left * right,
                b'/' if right != 0.0 => left / right,
                b'%' if right != 0.0 => left % right,
                b'^' => left.powf(right),
                _ => return None,
            };
            if !left.is_finite() {
                return None;
            }
        }
        Some(left)
    }
}

/// Accepts an explicit arithmetic expression; never treats ordinary prose as math.
pub(super) fn calculate(expression: &str) -> Option<String> {
    let text = expression.trim();
    if text.is_empty() || text.len() > MAX_LEN || !text.is_ascii() {
        return None;
    }
    let mut parser = Parser {
        bytes: text.as_bytes(),
        index: 0,
    };
    let result = parser.expr(0, 0)?;
    parser.skip_space();
    if parser.index != parser.bytes.len() || !result.is_finite() {
        return None;
    }
    let value = if result == 0.0 { 0.0 } else { result };
    if value.abs() >= 1e12 || (value != 0.0 && value.abs() < 1e-7) {
        return Some(format!("{value:.10e}"));
    }
    let precise = format!("{value:.10}");
    Some(precise.trim_end_matches('0').trim_end_matches('.').to_string())
}

#[cfg(test)]
mod tests {
    use super::calculate;

    #[test]
    fn arithmetic_works_without_network_or_ai() {
        for (input, result) in [
            ("2+2", "4"),
            ("2*(3+4)", "14"),
            ("-2^2", "-4"),
            ("2^3^2", "512"),
            ("7 % 4", "3"),
            ("1/4", "0.25"),
            ("3.5*2", "7"),
        ] {
            assert_eq!(calculate(input).as_deref(), Some(result), "{input}");
        }
    }

    #[test]
    fn malformed_and_dangerous_expressions_are_refused() {
        for bad in [
            "", "1/0", "10%", "1+", "2*(", "()", "2 + fn()", "NaN",
            "0^(-1)", "2..3", "1e", "((((((((((((((((((((((((((1))))))))))))))))))))))))))",
        ] {
            assert!(calculate(bad).is_none(), "{bad}");
        }
        assert!(calculate(&"1+".repeat(200)).is_none());
    }

    #[test]
    fn scientific_notation_is_supported() {
        assert_eq!(calculate("1e3 / 4").as_deref(), Some("250"));
    }
}
