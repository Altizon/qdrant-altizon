use std::collections::BinaryHeap;

use num_traits::float::FloatCore;

use super::entry_points::EntryPoints;
use crate::common::utils::rev_range;
use crate::index::visited_pool::VisitedPool;
use crate::spaces::tools::FixedLengthPriorityQueue;
use crate::types::{PointOffsetType, ScoreType};
use crate::vector_storage::{RawScorer, ScoredPointOffset};

pub struct GraphLinearBuilder<'a> {
    m: usize,
    m0: usize,
    ef_construct: usize,
    links_layers: Vec<Vec<PointOffsetType>>,
    entry_points: EntryPoints,
    visited_pool: VisitedPool,
    points_scorer: Box<dyn RawScorer + 'a>,
    point_levels: Vec<usize>,
}

pub struct GraphLinkRequest {
    point_id: PointOffsetType,
    level: usize,
    entry: ScoredPointOffset,
}

pub struct GraphLinkResponse {
    point_id: PointOffsetType,
    level: usize,
    entry: ScoredPointOffset,
    links: Vec<PointOffsetType>,
    neighbor_ids: Vec<PointOffsetType>,
    neighbor_links: Vec<Vec<PointOffsetType>>,
}

impl GraphLinkResponse {
    pub fn next_request(&self) -> Option<GraphLinkRequest> {
        if self.level > 0 {
            Some(GraphLinkRequest {
                point_id: self.point_id,
                level: self.level - 1,
                entry: self.entry,
            })
        } else {
            None
        }
    }
}

impl<'a> GraphLinearBuilder<'a> {
    pub fn new(
        levels: &[usize],
        m: usize,
        m0: usize,
        ef_construct: usize,
        entry_points_num: usize,
        points_scorer: Box<dyn RawScorer + 'a>,
    ) -> Self {
        let levels_count = levels.iter().copied().max().unwrap();
        let mut links_layers: Vec<Vec<PointOffsetType>> = vec![];
        for i in 0..=levels_count {
            let level_m = if i == 0 { m0 } else { m };
            let buffer = vec![0 as PointOffsetType; (level_m + 1) * levels.len()];
            links_layers.push(buffer);
        }

        Self {
            m,
            m0,
            ef_construct,
            links_layers,
            entry_points: EntryPoints::new(entry_points_num),
            visited_pool: VisitedPool::new(),
            points_scorer,
            point_levels: levels.to_vec(),
        }
    }

    pub fn apply_link_response(&mut self, response: &GraphLinkResponse) {
        self.set_links(response.point_id, response.level, &response.links);
        for (id, links) in response
            .neighbor_ids
            .iter()
            .zip(response.neighbor_links.iter())
        {
            self.set_links(*id, response.level, links);
        }
    }

    pub fn get_link_request(&mut self, point_id: PointOffsetType) -> Option<GraphLinkRequest> {
        let level = self.get_point_level(point_id);
        let entry_point_opt = self.entry_points.new_point(point_id, level, |_| true);
        match entry_point_opt {
            None => None,
            Some(entry_point) => {
                let entry = if entry_point.level > level {
                    self.search_entry(point_id, entry_point.point_id, entry_point.level, level)
                } else {
                    ScoredPointOffset {
                        idx: entry_point.point_id,
                        score: self.score(point_id, entry_point.point_id),
                    }
                };
                let level = std::cmp::min(level, entry_point.level);
                Some(GraphLinkRequest {
                    point_id,
                    level,
                    entry,
                })
            }
        }
    }

    pub fn link_new_point(&mut self, point_id: PointOffsetType) {
        let mut request = self.get_link_request(point_id);
        while let Some(r) = request {
            let response = self.link(r);
            self.apply_link_response(&response);
            request = response.next_request();
        }
    }

    pub fn link(&self, request: GraphLinkRequest) -> GraphLinkResponse {
        let nearest_points = self.search_on_level(request.point_id, request.entry, request.level);

        let mut response = GraphLinkResponse {
            point_id: request.point_id,
            level: request.level,
            entry: nearest_points
                .iter()
                .copied()
                .max()
                .unwrap_or(request.entry),
            links: vec![],
            neighbor_ids: vec![],
            neighbor_links: vec![],
        };
        let level_m = self.get_m(request.level);

        response.links =
            self.select_candidate_with_heuristic_from_sorted(&nearest_points.into_vec(), level_m);
        for &other_point in &response.links {
            response.neighbor_ids.push(other_point);

            let other_point_links = self.get_links(other_point, request.level);
            if other_point_links.len() < level_m {
                // If linked point is lack of neighbours
                let mut other_point_links = other_point_links.to_vec();
                other_point_links.push(request.point_id);
                response.neighbor_links.push(other_point_links);
            } else {
                let mut candidates = BinaryHeap::with_capacity(level_m + 1);
                candidates.push(ScoredPointOffset {
                    idx: request.point_id,
                    score: self.score(request.point_id, other_point),
                });
                for other_point_link in other_point_links.iter().take(level_m).copied() {
                    candidates.push(ScoredPointOffset {
                        idx: other_point_link,
                        score: self.score(other_point_link, other_point),
                    });
                }
                let mut candidates = candidates.into_sorted_vec();
                candidates.reverse();
                let selected_candidates =
                    self.select_candidate_with_heuristic_from_sorted(&candidates, level_m);
                response.neighbor_links.push(selected_candidates);
            }
        }
        response
    }

    /// <https://github.com/nmslib/hnswlib/issues/99>
    fn select_candidate_with_heuristic_from_sorted(
        &self,
        candidates: &[ScoredPointOffset],
        m: usize,
    ) -> Vec<PointOffsetType> {
        let mut result_list = vec![];
        result_list.reserve(m);
        for current_closest in candidates {
            if result_list.len() >= m {
                break;
            }
            let mut is_good = true;
            for &selected_point in &result_list {
                let dist_to_already_selected = self.score(current_closest.idx, selected_point);
                if dist_to_already_selected > current_closest.score {
                    is_good = false;
                    break;
                }
            }
            if is_good {
                result_list.push(current_closest.idx);
            }
        }

        result_list
    }

    fn search_on_level(
        &self,
        id: PointOffsetType,
        level_entry: ScoredPointOffset,
        level: usize,
    ) -> FixedLengthPriorityQueue<ScoredPointOffset> {
        let mut visited_list = self.visited_pool.get(self.num_points());
        visited_list.check_and_update_visited(level_entry.idx);

        let mut nearest = FixedLengthPriorityQueue::<ScoredPointOffset>::new(self.ef_construct);
        nearest.push(level_entry);
        let mut candidates = BinaryHeap::<ScoredPointOffset>::from_iter([level_entry]);

        while let Some(candidate) = candidates.pop() {
            let lower_bound = match nearest.top() {
                None => ScoreType::min_value(),
                Some(worst_of_the_best) => worst_of_the_best.score,
            };
            if candidate.score < lower_bound {
                break;
            }

            let links = self.get_links(candidate.idx, level);
            for &link in links.iter() {
                if !visited_list.check_and_update_visited(link) {
                    let score = self.score(link, id);
                    Self::process_candidate(
                        &mut nearest,
                        &mut candidates,
                        ScoredPointOffset { idx: link, score },
                    )
                }
            }
        }

        for &existing_link in self.get_links(id, level) {
            if !visited_list.check(existing_link) {
                Self::process_candidate(
                    &mut nearest,
                    &mut candidates,
                    ScoredPointOffset {
                        idx: existing_link,
                        score: self.score(id, existing_link),
                    },
                );
            }
        }

        self.visited_pool.return_back(visited_list);
        nearest
    }

    fn process_candidate(
        nearest: &mut FixedLengthPriorityQueue<ScoredPointOffset>,
        candidates: &mut BinaryHeap<ScoredPointOffset>,
        score_point: ScoredPointOffset,
    ) {
        let was_added = match nearest.push(score_point) {
            None => true,
            Some(removed) => removed.idx != score_point.idx,
        };
        if was_added {
            candidates.push(score_point);
        }
    }

    fn search_entry(
        &self,
        id: PointOffsetType,
        entry_point: PointOffsetType,
        top_level: usize,
        target_level: usize,
    ) -> ScoredPointOffset {
        let mut current_point = ScoredPointOffset {
            idx: entry_point,
            score: self.score(id, entry_point),
        };
        for level in rev_range(top_level, target_level) {
            let mut changed = true;
            while changed {
                changed = false;

                for &link in self.get_links(current_point.idx, level) {
                    let score = self.score(link, id);
                    if score > current_point.score {
                        changed = true;
                        current_point = ScoredPointOffset { idx: link, score };
                    }
                }
            }
        }
        current_point
    }

    fn get_m(&self, level: usize) -> usize {
        if level == 0 {
            self.m0
        } else {
            self.m
        }
    }

    fn get_point_level(&self, point_id: PointOffsetType) -> usize {
        self.point_levels[point_id as usize]
    }

    fn score(&self, a: PointOffsetType, b: PointOffsetType) -> ScoreType {
        self.points_scorer.score_internal(a, b)
    }

    fn num_points(&self) -> usize {
        self.point_levels.len()
    }

    pub fn get_links(&self, point_id: PointOffsetType, level: usize) -> &[PointOffsetType] {
        let level_m = self.get_m(level);
        let start_index = point_id as usize * (level_m + 1);
        let len = self.links_layers[level][start_index] as usize;
        &self.links_layers[level][start_index + 1..start_index + 1 + len]
    }

    pub fn set_links(
        &mut self,
        point_id: PointOffsetType,
        level: usize,
        links: &[PointOffsetType],
    ) {
        let level_m = self.get_m(level);
        let start_index = point_id as usize * (level_m + 1);
        self.links_layers[level][start_index] = links.len() as PointOffsetType;
        self.links_layers[level][start_index + 1..start_index + 1 + links.len()]
            .copy_from_slice(links);
    }
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    use super::*;
    use crate::fixtures::index_fixtures::{FakeFilterContext, TestRawScorerProducer};
    use crate::index::hnsw_index::graph_layers_builder::GraphLayersBuilder;
    use crate::index::hnsw_index::point_scorer::FilteredScorer;
    use crate::spaces::simple::CosineMetric;
    use crate::types::PointOffsetType;

    const M: usize = 8;

    #[test]
    fn test_equal_hnsw() {
        let num_vectors = 1000;
        let m = M;
        let ef_construct = 16;
        let entry_points_num = 10;

        let mut rng = StdRng::seed_from_u64(42);
        let vector_holder = TestRawScorerProducer::<CosineMetric>::new(16, num_vectors, &mut rng);

        let mut graph_layers_1 = GraphLayersBuilder::new_with_params(
            num_vectors,
            m,
            m * 2,
            ef_construct,
            entry_points_num,
            true,
            true,
        );

        let levels = (0..(num_vectors as PointOffsetType))
            .map(|idx| {
                let level = graph_layers_1.get_random_layer(&mut rng);
                graph_layers_1.set_levels(idx, level);
                level
            })
            .collect_vec();

        for idx in 0..(num_vectors as PointOffsetType) {
            let fake_filter_context = FakeFilterContext {};
            let added_vector = vector_holder.vectors.get(idx).to_vec();
            let raw_scorer = vector_holder.get_raw_scorer(added_vector.clone());

            let scorer = FilteredScorer::new(raw_scorer.as_ref(), Some(&fake_filter_context));
            graph_layers_1.link_new_point(idx, scorer);
        }

        let added_vector = vector_holder.vectors.get(0).to_vec();
        let raw_scorer = vector_holder.get_raw_scorer(added_vector.clone());
        let mut graph_layers_2 = GraphLinearBuilder::new(
            &levels,
            m,
            m * 2,
            ef_construct,
            entry_points_num,
            raw_scorer,
        );

        for idx in 0..(num_vectors as PointOffsetType) {
            graph_layers_2.link_new_point(idx);
        }

        for (point_id, links_1) in graph_layers_1.links_layers.iter().enumerate() {
            for (level, links_1) in links_1.iter().enumerate() {
                let links_1 = links_1.read().clone();
                let links_2 = graph_layers_2.get_links(point_id as PointOffsetType, level);
                assert_eq!(links_1.as_slice(), links_2);
            }
        }
    }
}