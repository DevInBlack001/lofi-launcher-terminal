use std::collections::BTreeMap;

// Title text is a far more deliberate signal of what a video is about than the
// description, which frequently carries generic filler (e.g. "atmosphere") regardless
// of genre. Weighting title occurrences higher keeps specific title keywords from
// losing to incidental description matches.
const TITLE_WEIGHT: usize = 3;
const DESCRIPTION_WEIGHT: usize = 1;

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

pub fn classify(classifier: &BTreeMap<String, Vec<String>>, title: &str, description: &str) -> Option<String> {
    let title = title.to_lowercase();
    let description = description.to_lowercase();

    let mut best: Option<(String, usize)> = None;
    for (mood, keywords) in classifier {
        let score: usize = keywords
            .iter()
            .map(|keyword| {
                let keyword = keyword.to_lowercase();
                count_occurrences(&title, &keyword) * TITLE_WEIGHT
                    + count_occurrences(&description, &keyword) * DESCRIPTION_WEIGHT
            })
            .sum();

        if score > 0 {
            match &best {
                Some((_, best_score)) if *best_score >= score => {}
                _ => best = Some((mood.clone(), score)),
            }
        }
    }

    best.map(|(mood, _)| mood)
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

    #[test]
    fn title_relevant_match_beats_generic_description_filler() {
        let mut classifier = BTreeMap::new();
        classifier.insert("ambient".to_string(), vec!["ambient".to_string(), "atmosphere".to_string()]);
        classifier.insert("code-and-chill".to_string(), vec!["code".to_string(), "synthwave".to_string()]);
        let result = classify(
            &classifier,
            "coding music synthwave beats to program to",
            "fuel your coding sessions with synthwave, creating the perfect atmosphere for deep work",
        );
        assert_eq!(result, Some("code-and-chill".to_string()));
    }

    #[test]
    fn multiple_title_keyword_occurrences_outweigh_a_single_description_filler_word() {
        let mut classifier = BTreeMap::new();
        classifier.insert("ambient".to_string(), vec!["atmosphere".to_string()]);
        classifier.insert("chill-beats".to_string(), vec!["chill".to_string(), "beats".to_string()]);
        let result = classify(
            &classifier,
            "90's Chill Lofi Study Music Lofi Rain Chillhop Beats",
            "relaxing atmosphere for study and sleep",
        );
        assert_eq!(result, Some("chill-beats".to_string()));
    }
}
