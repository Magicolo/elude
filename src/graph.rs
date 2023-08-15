use crate::{
    depend::Dependency,
    work::{self, Worker},
};

pub struct Graph<'a, 'b>(&'b Worker<'a>);
pub struct Node<'a, 'b>(usize, &'b work::Node<'a>, &'b Worker<'a>);
pub struct Cluster<'a, 'b>(usize, &'b work::Cluster, &'b Worker<'a>);

impl<'a, 'b> Graph<'a, 'b> {
    pub fn new(worker: &'b Worker<'a>) -> Self {
        Self(worker)
    }

    pub fn roots(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.0
            .roots
            .iter()
            .filter_map(|&node| Some(Node(node, self.0.nodes.get(node)?, self.0)))
    }

    pub fn nodes(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.0
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| Node(index, node, self.0))
    }

    pub fn clusters(&self) -> impl Iterator<Item = Cluster<'a, '_>> {
        self.0
            .clusters
            .iter()
            .enumerate()
            .map(|(index, cluster)| Cluster(index, cluster, self.0))
    }
}

impl<'a, 'b> Node<'a, 'b> {
    pub const fn index(&self) -> usize {
        self.0
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.1.dependencies
    }

    pub fn rivals(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.1
            .weak
            .rivals
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }

    pub fn previous(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.1
            .strong
            .previous
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }

    pub fn next(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.1
            .strong
            .next
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }
}

impl<'a, 'b> Cluster<'a, 'b> {
    pub const fn index(&self) -> usize {
        self.0
    }

    pub fn nodes(&self) -> impl Iterator<Item = Node<'a, '_>> {
        self.1
            .nodes
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }
}
