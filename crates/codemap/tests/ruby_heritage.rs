//! Ruby superclass names retain their written qualification and lexical scope.
use std::collections::BTreeSet;

use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

const SOURCE: &str = r"
class Base; end
class RootBase; end
class RootChild < RootBase; end
module Workbench
  class Base
    def inherited; 1; end
  end
  class RootBase; end
  module Nested
    class Base; end
    class Child < Base; end
  end
  class Qualified < Nested::Base; end
  class Absolute < ::RootBase; end
  class Holder < Base
    class Base; end
  end
end
class Workbench::Compact < Base; end
class Workbench::CompactNested < Nested::Base; end
module Decoy
  class Child; end
end
class Outside < Workbench::Base
  def run; inherited; end
end
class MissingChild < Absent::Base; end
module Collision
  class Base; end
end
module NotAClass; end
class WrongKindChild < NotAClass; end
class AmbiguousChild < Collision::Base; end
module Feature; end
module Package
  module Feature; end
  module Shared; end
  class Host
    include Feature
    prepend Shared
    extend ::Feature
  end
end
class Package::CompactHost
  include Feature
end
class QualifiedHost
  include Package::Feature
  prepend Package::Shared
  extend Package::Feature
end
class Other; end
class ReceiverHost
  Other.include Feature
  Other.prepend Feature
  Other.extend Feature
end
class ExplicitSelfHost
  self.include Feature
  self.extend Feature
end
class SelfPrependHost
  self.prepend Feature
end
module Util
  extend self
end
module Authorization
  include External::Authorization
end
class Extractor
  extend External::Extractor
end
class WrongMixinKind
  include RootBase
end
";

#[test]
fn heritage_edges_require_exact_lexical_class_and_module_constants() {
    let repo = common::make_repo_with(&[
        ("app.rb".into(), SOURCE.into()),
        (
            "reopened.rb".into(),
            "module Collision\n  class Base; end\nend\n".into(),
        ),
    ])
    .unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap();
    let symbol = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .symbol_key
            .split_once('#')
            .unwrap()
            .1
            .to_string()
    };
    let actual: BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Inherits | EdgeKind::Implements | EdgeKind::Extends
            )
        })
        .map(|edge| {
            (
                format!("{:?}", edge.kind),
                symbol(&edge.from),
                symbol(&edge.to),
            )
        })
        .collect();
    let expected: BTreeSet<_> = [
        (EdgeKind::Inherits, "RootChild", "RootBase"),
        (EdgeKind::Inherits, "Workbench::Compact", "Base"),
        (EdgeKind::Implements, "Package::CompactHost", "Feature"),
        (
            EdgeKind::Inherits,
            "Workbench::Nested::Child",
            "Workbench::Nested::Base",
        ),
        (
            EdgeKind::Inherits,
            "Workbench::Qualified",
            "Workbench::Nested::Base",
        ),
        (EdgeKind::Inherits, "Workbench::Absolute", "RootBase"),
        (EdgeKind::Inherits, "Workbench::Holder", "Workbench::Base"),
        (EdgeKind::Inherits, "Outside", "Workbench::Base"),
        (EdgeKind::Implements, "Package::Host", "Package::Feature"),
        (EdgeKind::Implements, "Package::Host", "Package::Shared"),
        (EdgeKind::Extends, "Package::Host", "Feature"),
        (EdgeKind::Implements, "QualifiedHost", "Package::Feature"),
        (EdgeKind::Implements, "QualifiedHost", "Package::Shared"),
        (EdgeKind::Extends, "QualifiedHost", "Package::Feature"),
        (EdgeKind::Implements, "ExplicitSelfHost", "Feature"),
        (EdgeKind::Extends, "ExplicitSelfHost", "Feature"),
        (EdgeKind::Implements, "SelfPrependHost", "Feature"),
    ]
    .into_iter()
    .map(|(kind, from, to)| (format!("{kind:?}"), from.to_string(), to.to_string()))
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn reopened_receivers_bind_heritage_to_the_exact_source_declaration() {
    let first = "class Base; end\nmodule Feature; end\nmodule Priority; end\nmodule ClassFeature; end\nclass Host < Base\n  include Feature\nend\nmodule Group\n  include Feature\nend\nclass Local < Base\n  include Feature\nend\nclass Local\n  prepend Priority\n  extend ClassFeature\nend\n";
    let second = "class Host\n  prepend Priority\n  extend ClassFeature\nend\nmodule Group\n  extend ClassFeature\nend\n";
    let repo = common::make_repo_with(&[
        ("first.rb".into(), first.into()),
        ("second.rb".into(), second.into()),
    ])
    .unwrap();
    let options = ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    };
    let snapshot = extract(&options).unwrap();
    let node = |id: &str| snapshot.nodes.iter().find(|node| node.id == id).unwrap();
    let actual: BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Inherits | EdgeKind::Implements | EdgeKind::Extends
            )
        })
        .map(|edge| {
            let from = node(&edge.from);
            assert_eq!(from.provenance.rel_path, edge.provenance.rel_path);
            assert!(from.provenance.span.start_byte <= edge.provenance.span.start_byte);
            assert!(edge.provenance.span.end_byte <= from.provenance.span.end_byte);
            (
                format!("{:?}", edge.kind),
                from.provenance.rel_path.as_str(),
                from.provenance.span.start_byte,
                node(&edge.to).name.as_str(),
            )
        })
        .collect();
    let local_reopened = first.rfind("class Local").unwrap();
    let expected: BTreeSet<_> = [
        (
            EdgeKind::Inherits,
            "first.rb",
            first.find("class Host").unwrap(),
            "Base",
        ),
        (
            EdgeKind::Implements,
            "first.rb",
            first.find("class Host").unwrap(),
            "Feature",
        ),
        (
            EdgeKind::Implements,
            "first.rb",
            first.find("module Group").unwrap(),
            "Feature",
        ),
        (
            EdgeKind::Inherits,
            "first.rb",
            first.find("class Local").unwrap(),
            "Base",
        ),
        (
            EdgeKind::Implements,
            "first.rb",
            first.find("class Local").unwrap(),
            "Feature",
        ),
        (EdgeKind::Implements, "first.rb", local_reopened, "Priority"),
        (
            EdgeKind::Extends,
            "first.rb",
            local_reopened,
            "ClassFeature",
        ),
        (EdgeKind::Implements, "second.rb", 0, "Priority"),
        (EdgeKind::Extends, "second.rb", 0, "ClassFeature"),
        (
            EdgeKind::Extends,
            "second.rb",
            second.find("module Group").unwrap(),
            "ClassFeature",
        ),
    ]
    .into_iter()
    .map(|(kind, path, start, target)| (format!("{kind:?}"), path, start, target))
    .collect();
    assert_eq!(actual, expected);
    assert_eq!(snapshot, extract(&options).unwrap());
}

#[test]
fn reopened_receivers_do_not_disambiguate_heritage_targets() {
    let repo = common::make_repo_with(&[
        (
            "first.rb".into(),
            "class Base; end\nmodule Feature; end\nclass Host < Base\n  include Feature\n  prepend Missing\n  extend External::Feature\n  include Base\nend\nclass WrongKind < Feature; end\n".into(),
        ),
        (
            "second.rb".into(),
            "class Base; end\nmodule Feature; end\nclass Host; end\nclass WrongKind; end\n".into(),
        ),
    ])
    .unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap();
    assert!(!snapshot.edges.iter().any(|edge| matches!(
        edge.kind,
        EdgeKind::Inherits | EdgeKind::Implements | EdgeKind::Extends
    )));
}

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn inherited_r2_call_requires_the_qualified_superclass_edge() {
    let repo = common::make_repo_with(&[("app.rb".into(), SOURCE.into())]).unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: true,
        measures: false,
        twins: false,
    })
    .unwrap();
    let symbol = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap()
            .symbol_key
            .split_once('#')
            .unwrap()
            .1
            .to_string()
    };
    let calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| (symbol(&edge.from), symbol(&edge.to)))
        .collect();
    assert_eq!(
        calls,
        [(
            "Outside::run".to_string(),
            "Workbench::Base::inherited".to_string()
        )]
    );
}
