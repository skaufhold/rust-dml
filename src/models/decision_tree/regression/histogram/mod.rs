use self::loss_functions::*;
use data::serialization::Serializable;
use models::decision_tree::histogram_generics::*;
use models::decision_tree::tree::NodeIndex;
use models::decision_tree::tree::Rule;
use num_traits::Float;
use ordered_float::OrderedFloat;
use std::cmp::Ordering;
use std::collections::binary_heap::BinaryHeap;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound::{Excluded, Included, Unbounded};

mod loss_functions;
pub mod operators;

pub type TargetValueHistogramSet<T, L> =
    BTreeHistogramSet<NodeIndex, BTreeHistogramSet<usize, FnvHistogramSet<T, Histogram<L>>>>;

pub trait FindSplits<T> {
    fn find_best_splits(&self, nodes: &[NodeIndex]) -> Vec<(NodeIndex, Rule<T>)>;
}

impl<T: DiscreteValue, L: ContinuousValue> FindSplits<T> for TargetValueHistogramSet<T, L> {
    fn find_best_splits(&self, nodes: &[NodeIndex]) -> Vec<(NodeIndex, Rule<T>)> {
        nodes
            .iter()
            .map(|n| (n, self.get(n).expect("Select leaf node for split finding")))
            .map(|(node, node_histograms)| {
                let (attr, x_subset, _loss) = node_histograms
                    .iter()
                    .map(|(attribute, attr_histograms)| {
                        // sort feature values according to their median values
                        let mut sorted_feature_values = attr_histograms
                            .iter()
                            .map(|(x, histogram)| {
                                (x, histogram, OrderedFloat::from(histogram.median()))
                            })
                            .collect::<Vec<_>>();
                        sorted_feature_values
                            .as_mut_slice()
                            .sort_unstable_by_key(|(_, _, median)| median.clone());

                        let merged_attribute_hist = attr_histograms
                            .summarize()
                            .expect("merge all attribute histograms");

                        let (x_subset, min_loss) = attr_histograms
                            .iter()
                            .map(|(x_trial, _)| {
                                let split_index = sorted_feature_values
                                    .iter()
                                    .position(|(x, _, _)| **x == *x_trial)
                                    .expect("Find trial feature value in sorted set");
                                let (left_slice, right_slice) =
                                    sorted_feature_values.split_at(split_index);
                                let left_split = left_slice
                                    .iter()
                                    .map(|(_, hist, _)| *hist)
                                    .summarize()
                                    .expect("summarize left split histograms");
                                let right_split = right_slice
                                    .iter()
                                    .map(|(_, hist, _)| *hist)
                                    .summarize()
                                    .expect("summarize right split histograms");

                                (
                                    left_slice.iter().map(|(x, _, _)| **x).collect::<Vec<T>>(),
                                    OrderedFloat::from(Histogram::weighted_loss(
                                        &merged_attribute_hist,
                                        &left_split,
                                        &right_split,
                                    )),
                                )
                            })
                            .min_by(|(_x1, loss1), (_x2, loss2)| loss1.cmp(loss2))
                            .unwrap();
                        (attribute, x_subset, min_loss)
                    })
                    .min_by(|(_, _, loss1), (_, _, loss2)| loss1.cmp(loss2))
                    .unwrap();

                (*node, Rule::subset(*attr, x_subset))
            })
            .collect()
    }
}

pub trait FindNodeLabel<L> {
    fn find_node_label(&self, node: &NodeIndex) -> Option<L>;
}

impl<T: DiscreteValue, L: ContinuousValue> FindNodeLabel<L> for TargetValueHistogramSet<T, L> {
    fn find_node_label(&self, node: &NodeIndex) -> Option<L> {
        let node_histogram = self
            .get(node)?
            .iter()
            .flat_map(|(_k, h)| h.iter().map(|(_k, h)| h))
            .summarize()?;
        Some(node_histogram.median())
    }
}

/// Histogram describing the target value distribution at a certain tree node
#[derive(Clone)]
pub struct Histogram<L: Float> {
    bins: BTreeMap<BinAddress<L>, BinData<L>>,
    distances: BinaryHeap<BinDistance<L>>,
    n_bins: usize,
}

impl<L: ContinuousValue> BaseHistogram<L> for Histogram<L> {
    type Bin = (BinAddress<L>, BinData<L>);

    fn new(n_bins: usize) -> Self {
        Histogram {
            n_bins,
            distances: BinaryHeap::new(),
            bins: BTreeMap::new(),
        }
    }

    fn insert(&mut self, y: L) {
        let new_bin_data = BinData::init(y);
        let new_bin_address = BinAddress::init(y);
        let mut found = false;
        let before = self
            .bins
            .range_mut((Unbounded, Included(new_bin_address.clone())))
            .next_back()
            .and_then(|(addr, data)| {
                if addr.right >= new_bin_address.right {
                    data.count += 1;
                    data.sum = data.sum + y;
                    found = true;
                    None
                } else {
                    Some(BinDistance::new(addr, &new_bin_address))
                }
            });

        if !found {
            before.map(|dist| self.distances.push(dist));
            self.bins
                .range((Excluded(new_bin_address.clone()), Unbounded))
                .next()
                .map(|(addr, _)| BinDistance::new(&new_bin_address, addr))
                .map(|dist| self.distances.push(dist));
            self.bins.insert(new_bin_address, new_bin_data);
        }
        self.shrink_to_fit();
    }

    fn count(&self) -> u64 {
        self.bins.iter().fold(0, |sum, (_, d)| sum + d.count)
    }

    fn median(&self) -> L {
        let mut count_target = self.count() / 2;
        let (bin_addr, bin_data) = self
            .bins
            .iter()
            .skip_while(|(_, data)| {
                let done = count_target < data.count;
                if !done {
                    count_target -= data.count;
                }
                done
            })
            .next()
            .unwrap();
        let ratio = L::from(count_target).unwrap() / L::from(bin_data.count).unwrap();
        let l = bin_addr.left.into_inner();
        let r = bin_addr.right.into_inner();
        l + (r - l) * ratio
    }
}

impl<L: ContinuousValue> Serializable for Histogram<L> {
    /// Serializable representation of this histogram set
    type Serializable = (usize, Vec<(L, L, BinData<L>)>);

    /// Turn this item into a serializable version of itself
    fn into_serializable(self) -> Self::Serializable {
        let n_bins = self.n_bins;
        let bins = self
            .bins
            .into_iter()
            .map(|(address, data)| (address.left.into_inner(), address.right.into_inner(), data))
            .collect();
        (n_bins, bins)
    }

    /// Recover a item from its serializable representation
    fn from_serializable(serializable: Self::Serializable) -> Self {
        let mut histogram = Histogram::new(serializable.0);
        for (left, right, data) in serializable.1 {
            histogram
                .bins
                .insert(BinAddress::new(left.into(), right.into()), data);
            histogram.rebuild_distances();
        }
        histogram
    }
}

impl<L: ContinuousValue> HistogramSetItem for Histogram<L> {
    /// Merge another instance of this type into this histogram
    fn merge(&mut self, other: Self) {
        for (new_addr, new_data) in other.bins {
            self.bins
                .entry(new_addr)
                .and_modify(|bin| bin.merge(&new_data))
                .or_insert(new_data);
        }
        self.rebuild_distances();
        self.shrink_to_fit();
    }

    fn merge_borrowed(&mut self, other: &Self) {
        for (new_addr, new_data) in &other.bins {
            self.bins
                .entry(new_addr.clone())
                .and_modify(|bin| bin.merge(&new_data))
                .or_insert(new_data.clone());
        }
        self.rebuild_distances();
        self.shrink_to_fit();
    }

    fn empty_clone(&self) -> Self {
        Self::new(self.n_bins)
    }
}

impl<L: Float> Histogram<L>
where
    BinAddress<L>: Ord,
{
    fn shrink_to_fit(&mut self) {
        while self.bins.len() > self.n_bins {
            // find two closest together bins
            let least_diff = self.distances.pop().unwrap();

            let data_l = self.bins.remove(&least_diff.left);
            let data_r = self.bins.remove(&least_diff.right);

            // there may be "out of date" distances on the heap if one of the bins was merged
            if data_l.is_none() || data_r.is_none() {
                if let Some(l) = data_l {
                    self.bins.insert(least_diff.left, l);
                }
                if let Some(r) = data_r {
                    self.bins.insert(least_diff.right, r);
                }
                continue;
            }

            let (mut merged_addr, addr_r) = (least_diff.left, least_diff.right);
            merged_addr.merge(&addr_r);

            let mut merged_data = data_l.unwrap();
            merged_data.merge(&data_r.unwrap());
            self.bins.insert(merged_addr.clone(), merged_data);

            // insert updated distances after merge
            self.bins
                .range((Excluded(&merged_addr), Unbounded))
                .next()
                .map(|(after_addr, _)| BinDistance::new(&merged_addr, after_addr))
                .map(|dist| self.distances.push(dist));
        }
    }

    fn rebuild_distances(&mut self) {
        self.distances.clear();
        for (left, right) in self.bins.keys().zip(self.bins.keys().skip(1)) {
            self.distances.push(BinDistance::new(left, right));
        }
    }

    pub fn bins(&self) -> &BTreeMap<BinAddress<L>, BinData<L>> {
        &self.bins
    }
}

impl<L: Float + fmt::Debug> fmt::Debug for Histogram<L> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Bins:\n")?;
        for (addr, bin) in &self.bins {
            fmt::Display::fmt(
                &format!(
                    "{:?}/{:?} -> {:?}\n",
                    addr.left.into_inner(),
                    addr.right.into_inner(),
                    bin
                ),
                fmt,
            )?;
        }
        write!(fmt, "Distances:\n")?;
        for dist in &self.distances {
            fmt::Display::fmt(&format!("{:?}\n", dist), fmt)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BinAddress<L: Float> {
    pub left: OrderedFloat<L>,
    pub right: OrderedFloat<L>,
}

impl<L: Float> Eq for BinAddress<L> {}

impl<L: Float> Ord for BinAddress<L> {
    fn cmp(&self, other: &Self) -> Ordering {
        let left_ord = self.left.cmp(&other.left);
        let right_ord = self.right.cmp(&other.right);
        left_ord.then(right_ord)
    }
}

impl<L: Float> PartialOrd for BinAddress<L> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<L: Float> BinAddress<L> {
    pub fn init(y: L) -> Self {
        BinAddress {
            left: y.into(),
            right: y.into(),
        }
    }

    pub fn new(left: L, right: L) -> Self {
        BinAddress {
            left: left.into(),
            right: right.into(),
        }
    }

    pub fn contains(&self, y: L) -> Ordering {
        let y_ord = OrderedFloat::from(y);
        if y_ord < self.left {
            Ordering::Less
        } else if y_ord > self.right {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// Merges this bin with another one, summing the number of points
    /// and shifting the center of the bin to accomodate
    pub fn merge(&mut self, other: &Self) {
        self.left = OrderedFloat::min(self.left, other.left);
        self.right = OrderedFloat::max(self.right, other.right);
    }
}

#[derive(Debug, Clone, PartialEq, Abomonation)]
pub struct BinData<L> {
    count: u64,
    sum: L,
}

impl<L: Float> BinData<L> {
    pub fn init(y: L) -> Self {
        BinData { count: 1, sum: y }
    }

    pub fn new(count: u64, sum: L) -> Self {
        BinData { count, sum }
    }

    /// Merges this bin with another one, summing the number of points
    /// and shifting the center of the bin to accomodate
    pub fn merge(&mut self, other: &Self) {
        self.sum = self.sum + other.sum;
        self.count += other.count;
    }
}

trait PartialBinSum<L> {
    /// Estimates an R-partial sum of this bin, where R is any number
    /// between 0 and the number of samples contained in the bin
    /// panics if R is greater than the count of items in the bin
    fn partial_sum(&self, r: u64) -> L;
}

impl<'a, 'b, L: Float> PartialBinSum<L> for (&'a BinAddress<L>, &'b BinData<L>) {
    /// Estimates an R-partial sum of this bin, where R is any number
    /// between 0 and the number of samples contained in the bin
    /// panics if R is greater than the count of items in the bin
    fn partial_sum(&self, r: u64) -> L {
        let (addr, data) = self;
        let r_float = L::from(r).unwrap();
        if r < data.count {
            let count_float = L::from(data.count).unwrap();
            let two = L::from(2.).unwrap();
            let delta = (data.sum - *addr.right - count_float * *addr.left + *addr.left)
                / ((count_float - two) * (count_float - L::one()));
            r_float * *addr.left + r_float * (r_float - L::one()) * delta
        } else if r == data.count {
            data.sum
        } else {
            panic!("Attempt to calculate R-Partial sum where R > bin.count")
        }
    }
}

#[derive(Clone, Abomonation)]
struct BinDistance<L: Float> {
    pub left: BinAddress<L>,
    pub right: BinAddress<L>,
    pub distance: L,
}

impl<L: Float> BinDistance<L> {
    pub fn new(left: &BinAddress<L>, right: &BinAddress<L>) -> Self {
        BinDistance {
            left: left.clone(),
            right: right.clone(),
            distance: (right.left.into_inner() - left.right.into_inner()).max(L::from(0.).unwrap()),
        }
    }
}

// Implement reverse ordering for BinDistance so it can be used in a max-heap

impl<L: Float> PartialEq for BinDistance<L> {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl<L: Float> Eq for BinDistance<L> {}

impl<L: Float> PartialOrd for BinDistance<L> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.distance
            .partial_cmp(&other.distance)
            .and_then(|o| Some(o.reverse()))
    }
}

impl<L: Float> Ord for BinDistance<L> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Less)
    }
}

impl<L: Float + fmt::Debug> fmt::Debug for BinDistance<L> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        format!(
            "{:?}/{:?} -> {:?} -> {:?}/{:?}",
            self.left.left, self.left.right, self.distance, self.right.left, self.right.right
        ).fmt(fmt)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn insert() {
        let mut histogram = Histogram::new(3);
        let items = vec![1., 1., 2., 3.5, 2.1, 3.6];
        for i in items.into_iter() {
            histogram.insert(i);
        }
        assert_eq!(
            histogram.bins().iter().collect::<Vec<_>>(),
            vec![
                (&BinAddress::new(1.0, 1.0), &BinData::new(2, 2.0)),
                (&BinAddress::new(2.0, 2.1), &BinData::new(2, 4.1)),
                (&BinAddress::new(3.5, 3.6), &BinData::new(2, 7.1)),
            ]
        )
    }

    #[test]
    fn merge() {
        let mut h1 = Histogram::new(3);
        vec![1., 1.5, 3., 4., 4.5, 6.]
            .into_iter()
            .for_each(|i| h1.insert(i));

        let mut h2 = Histogram::new(3);
        vec![1.0, 7.0, 5.0].into_iter().for_each(|i| h2.insert(i));
        h1.merge_borrowed(&h2);

        assert_eq!(
            h1.bins().iter().collect::<Vec<_>>(),
            vec![
                (&BinAddress::new(1.0, 3.0), &BinData::new(4, 6.5)),
                (&BinAddress::new(4.0, 5.0), &BinData::new(3, 13.5)),
                (&BinAddress::new(6.0, 7.0), &BinData::new(2, 13.0)),
            ]
        )
    }
}

/*pub fn merge_histogram(
        &mut self,
        node: NodeIndex,
        attribute: usize,
        x: T,
        histogram: Histogram<L>,
    ) {
        match self
            .histograms
            .entry(node)
            .or_insert_with(FnvHashMap::default)
            .entry(attribute)
            .or_insert_with(FnvHashMap::default)
            .entry(x)
        {
            Occupied(mut entry) => {
                entry.get_mut().merge(histogram);
            }
            Vacant(entry) => {
                entry.insert(histogram);
            }
        }
    }

    pub fn merge_set(&mut self, mut other: Self) {
        for (node, mut other_histograms_by_node) in other.histograms.drain() {
            let node_map = self
                .histograms
                .entry(node)
                .or_insert_with(FnvHashMap::default);
            for (attribute, mut other_histograms_by_attr) in other_histograms_by_node.drain() {
                let attr_map = node_map
                    .entry(attribute)
                    .or_insert_with(FnvHashMap::default);
                for (x, mut other_histogram) in other_histograms_by_attr.drain() {
                    match attr_map.entry(x) {
                        Occupied(mut histogram_entry) => {
                            histogram_entry.get_mut().merge(other_histogram);
                        }
                        Vacant(empty_entry) => {
                            empty_entry.insert(other_histogram);
                        }
                    }
                }
            }
        }
    }

    pub fn attributes(&self) -> &usize {
        &self.attributes
    }

    pub fn into_vec(self) -> Vec<(NodeIndex, usize, T, Histogram<L>)> {
        unimplemented!()
    }*/