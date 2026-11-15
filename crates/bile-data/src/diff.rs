// TODO: Add diff producing API that takes a diff and a range of continues diffs.
// No diff allowed to be missing, not as in continous bile state ids, but as is
// there never being a dirty inbetween the 2 diffs.

// The way it will work is that storage types can query this (it is a producer node)
// and then pass a data by mut ref and the diffs by value to the node. Either the
// node that instant edit the data and return true or not edit the ref and make a copy
// then return false. This is so that they might be async work to apply the diff.

// The logging may need some thinking about unless a solution that fixes the problem
// fully, without comparmising on quality/principal. The issue is that the old
// producer that created the data, before the applyed diffs, owned the data and updated
// it. Now it still has partial ownership as it still is getting passed diffs but the
// diff applyer might also have made requests (if it returned false) and so it also has
// partial ownership. The most correct thing would be to have the ownership be passed
// through the diff applyer (Such that drops/diffs come from the OG to the diff applyer
// then out. This can happen if ever a requester requests for data of the same datatype
// as it is producing (I think this can be valid A needs a subset of A which could be
// the same type just from a diffrent node). The thing to note here is that they both
// have the same param and so we probably would need spetial code to deal with this
// type of situation. Which is why I dont really want to do it because special code
// is bad and a source of bugs. A param should peobably not depend on its self.
// This might be fixable if we dont let diff applying use external data but idk.
