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

/// Lists each offending input with why it was rejected.
pub fn print_failures<K: AsRef<str>>(failures: &[(K, String)]) {
    print_summary(
        &failures
            .iter()
            .map(|(input, reason)| (input.as_ref(), reason as &dyn ToString))
            .collect::<Vec<_>>(),
    );
}
