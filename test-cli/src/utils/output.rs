pub fn print_summary(results: &[(&str, &dyn ToString)]) {
    let max_key_len = results
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or_else(|| panic!("no summary data provided"));

    for (k, v) in results {
        println!("{:>max_key_len$} = {}", k, v.to_string())
    }
}
