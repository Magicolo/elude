use crate::{
    depend::Dependency,
    work::{self, Worker},
};

pub struct Graph<'a, 'b, S>(&'b Worker<'a, S>);
pub struct Node<'a, 'b, S>(usize, &'b work::Node<'a, S>, &'b Worker<'a, S>);
pub struct Cluster<'a, 'b, S>(usize, &'b work::Cluster, &'b Worker<'a, S>);

impl<'a, 'b, S> Graph<'a, 'b, S> {
    pub fn new(worker: &'b Worker<'a, S>) -> Self {
        Self(worker)
    }

    pub fn roots(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.0
            .roots
            .iter()
            .filter_map(|&node| Some(Node(node, self.0.nodes.get(node)?, self.0)))
    }

    pub fn nodes(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.0
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| Node(index, node, self.0))
    }

    pub fn clusters(&self) -> impl Iterator<Item = Cluster<'a, '_, S>> {
        self.0
            .clusters
            .iter()
            .enumerate()
            .map(|(index, cluster)| Cluster(index, cluster, self.0))
    }
}

impl<'a, 'b, S> Node<'a, 'b, S> {
    pub const fn index(&self) -> usize {
        self.0
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.1.dependencies
    }

    pub fn rivals(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.1
            .weak
            .rivals
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }

    pub fn previous(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.1
            .strong
            .previous
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }

    pub fn next(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.1
            .strong
            .next
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }
}

impl<'a, 'b, S> Cluster<'a, 'b, S> {
    pub const fn index(&self) -> usize {
        self.0
    }

    pub fn nodes(&self) -> impl Iterator<Item = Node<'a, '_, S>> {
        self.1
            .nodes
            .iter()
            .filter_map(|&node| Some(Node(node, self.2.nodes.get(node)?, self.2)))
    }
}
