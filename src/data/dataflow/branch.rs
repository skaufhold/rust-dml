use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::generic::builder_rc::OperatorBuilder;
use timely::dataflow::{Scope, Stream};
use timely::Data;

/// Extension trait for `Stream`.
pub trait Branch<S: Scope, D: Data> {
    /// Takes one input stream and splits it into two output streams.
    /// For each record, the supplied closure is called with a reference to
    /// the data and its time. If it returns true, the record will be sent
    /// to the second returned stream, otherwise it will be sent to the first.
    ///
    /// If the result of the closure only depends on the time, not the data,
    /// `branch_when` should be used instead.
    fn branch(
        &self,
        condition: impl Fn(&S::Timestamp, &D) -> bool + 'static,
    ) -> (Stream<S, D>, Stream<S, D>);
}

impl<S: Scope, D: Data> Branch<S, D> for Stream<S, D> {
    fn branch(
        &self,
        condition: impl Fn(&S::Timestamp, &D) -> bool + 'static,
    ) -> (Stream<S, D>, Stream<S, D>) {
        let mut builder = OperatorBuilder::new("Branch".to_owned(), self.scope());

        let mut input = builder.new_input(self, Pipeline);
        let (mut output1, stream1) = builder.new_output();
        let (mut output2, stream2) = builder.new_output();

        builder.build(move |_| {
            move |_frontiers| {
                let mut output1_handle = output1.activate();
                let mut output2_handle = output2.activate();

                input.for_each(|time, data| {
                    let mut out1 = output1_handle.session(&time);
                    let mut out2 = output2_handle.session(&time);
                    for datum in data.drain(..) {
                        if condition(&time.time(), &datum) {
                            out2.give(datum);
                        } else {
                            out1.give(datum);
                        }
                    }
                });
            }
        });

        (stream1, stream2)
    }
}

/// Extension trait for `Stream`.
pub trait BranchWhen<S: Scope, D: Data> {
    /// Takes one input stream and splits it into two output streams.
    /// For each time, the supplied closure is called. If it returns true,
    /// the records for that will be sent to the second returned stream, otherwise
    /// they will be sent to the first.
    fn branch_when(
        &self,
        condition: impl Fn(&S::Timestamp) -> bool + 'static,
    ) -> (Stream<S, D>, Stream<S, D>);
}

impl<S: Scope, D: Data> BranchWhen<S, D> for Stream<S, D> {
    fn branch_when(
        &self,
        condition: impl Fn(&S::Timestamp) -> bool + 'static,
    ) -> (Stream<S, D>, Stream<S, D>) {
        let mut builder = OperatorBuilder::new("Branch".to_owned(), self.scope());

        let mut input = builder.new_input(self, Pipeline);
        let (mut output1, stream1) = builder.new_output();
        let (mut output2, stream2) = builder.new_output();

        builder.build(move |_| {
            move |_frontiers| {
                let mut output1_handle = output1.activate();
                let mut output2_handle = output2.activate();

                input.for_each(|time, data| {
                    let mut out = if condition(&time.time()) {
                        output2_handle.session(&time)
                    } else {
                        output1_handle.session(&time)
                    };
                    out.give_content(data);
                });
            }
        });

        (stream1, stream2)
    }
}
