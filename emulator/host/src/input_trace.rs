//! Observations of the panel UART trace.

/// Count an exact panel UART event marker without interpreting adjacent bytes.
pub fn panel_uart_count(trace: &str, byte: u8) -> usize {
    trace
        .matches(&format!("l6 panel USART1 TX {byte:02x}\n"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_exact_lines_even_with_prefix_text() {
        let trace = "l6 panel USART1 TX 91\nl6 panel USART1 TX 910\nprefix l6 panel USART1 TX 91\nl6 panel USART1 TX 90\n";
        assert_eq!(panel_uart_count(trace, 0x91), 2);
        assert_eq!(panel_uart_count(trace, 0x90), 1);
    }
}
