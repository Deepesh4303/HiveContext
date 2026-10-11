/// Vector Cosine Similarity and Hybrid Ranking Engine
/// Ported and adapted from D:\Vector Embed\core\tokenizer\similarity.cpp and hybrid_ranker.cpp

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScoredItem<T> {
    pub item: T,
    pub score: f32,
    pub keyword_score: f32,
    pub vector_score: f32,
}

pub struct Similarity;

impl Similarity {
    /// Computes cosine similarity between two vector embeddings
    /// Returns a value between -1.0 and 1.0 (or 0.0 to 1.0 for normalized embeddings)
    pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }

        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;

        for (x, y) in a.iter().zip(b.iter()) {
            dot += x * y;
            norm_a += x * x;
            norm_b += y * y;
        }

        let denominator = norm_a.sqrt() * norm_b.sqrt();
        if denominator < 1e-9 {
            0.0
        } else {
            dot / denominator
        }
    }
}

pub struct HybridRanker {
    /// Balance between keyword score and vector similarity (0.0 = pure vector, 1.0 = pure keyword)
    pub alpha: f32,
}

impl HybridRanker {
    pub fn new(alpha: f32) -> Self {
        Self {
            alpha: alpha.clamp(0.0, 1.0),
        }
    }

    /// Computes a normalized keyword overlap score between query and target text
    pub fn keyword_score(query: &str, target_text: &str) -> f32 {
        let query_terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if query_terms.is_empty() {
            return 0.0;
        }

        let target_lower = target_text.to_lowercase();
        let mut hits = 0;
        for term in &query_terms {
            if target_lower.contains(term) {
                hits += 1;
            }
        }

        hits as f32 / query_terms.len() as f32
    }

    /// Combines keyword score and vector similarity using alpha weighting
    pub fn combine_score(
        &self,
        keyword_score: f32,
        query_vec: Option<&[f32]>,
        doc_vec: Option<&[f32]>,
    ) -> (f32, f32) {
        let vec_score = match (query_vec, doc_vec) {
            (Some(q), Some(d)) => {
                let raw_cos = Similarity::cosine(q, d);
                // Normalize cosine from [-1.0, 1.0] to [0.0, 1.0]
                ((raw_cos + 1.0) / 2.0).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };

        let final_score = if query_vec.is_some() && doc_vec.is_some() {
            self.alpha * keyword_score + (1.0 - self.alpha) * vec_score
        } else {
            // Fallback to pure keyword if either vector is missing
            keyword_score
        };

        (final_score, vec_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![1.0, 0.0, 0.0];
        let v3 = vec![0.0, 1.0, 0.0];

        assert!((Similarity::cosine(&v1, &v2) - 1.0).abs() < 1e-5);
        assert!(Similarity::cosine(&v1, &v3).abs() < 1e-5);
    }

    #[test]
    fn test_hybrid_ranking() {
        let ranker = HybridRanker::new(0.3); // 30% keyword, 70% vector
        let (score, vec_score) = ranker.combine_score(
            1.0,
            Some(&[1.0, 0.0]),
            Some(&[1.0, 0.0]),
        );
        assert!((score - 1.0).abs() < 1e-4);
        assert!((vec_score - 1.0).abs() < 1e-4);
    }
}
