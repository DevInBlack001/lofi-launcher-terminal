use std::collections::BTreeMap;

pub fn classify(classifier: &BTreeMap<String, Vec<String>>, title: &str, description: &str) -> Option<String> {
    let haystack = format!("{title} {description}").to_lowercase();
    for (mood, keywords) in classifier {
        for keyword in keywords {
            if haystack.contains(&keyword.to_lowercase()) {
                return Some(mood.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_classifier() -> BTreeMap<String, Vec<String>> {
        let mut m = BTreeMap::new();
        m.insert("rainy-day".to_string(), vec!["rain".to_string(), "storm".to_string()]);
        m.insert("ambient".to_string(), vec!["ambient".to_string(), "drone".to_string()]);
        m
    }

    #[test]
    fn matches_keyword_in_title_case_insensitively() {
        let result = classify(&sample_classifier(), "Heavy RAIN sounds for sleep", "");
        assert_eq!(result, Some("rainy-day".to_string()));
    }

    #[test]
    fn matches_keyword_in_description_when_title_has_none() {
        let result = classify(&sample_classifier(), "3 hour mix", "a deep ambient drone soundscape");
        assert_eq!(result, Some("ambient".to_string()));
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let result = classify(&sample_classifier(), "Upbeat pop music", "dance tracks");
        assert_eq!(result, None);
    }
}
