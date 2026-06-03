#[derive(Clone, Copy, PartialEq, Debug)]
enum Status {
    InProgress,
    InReview,
    Todo,
    Inbox,
    Done,
    Cancelled,
}

#[derive(Clone)]
struct Task {
    status: Status,
    id: i32,
}

const GROUP_ORDER: &[Status] = &[
    Status::InProgress,
    Status::InReview,
    Status::Todo,
    Status::Inbox,
    Status::Done,
    Status::Cancelled,
];

fn main() {
    let mut ts = Vec::new();
    for i in 0..1000 {
        ts.push(Task {
            status: GROUP_ORDER[i % 6],
            id: i as i32,
        });
    }

    // Old
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = GROUP_ORDER
            .iter()
            .filter_map(|&s| {
                let items: Vec<Task> = ts.iter().filter(|t| t.status == s).cloned().collect();
                if items.is_empty() {
                    None
                } else {
                    Some((s, items))
                }
            })
            .collect::<Vec<(Status, Vec<Task>)>>();
    }
    println!("Old time: {:?}", start.elapsed());

    // New
    let start2 = std::time::Instant::now();
    for _ in 0..1000 {
        let mut buckets: [Vec<Task>; 6] = Default::default();
        for t in ts.iter() {
            if let Some(idx) = GROUP_ORDER.iter().position(|&s| s == t.status) {
                buckets[idx].push(t.clone());
            }
        }

        let _ = GROUP_ORDER
            .iter()
            .copied()
            .zip(buckets)
            .filter_map(|(s, items)| {
                if items.is_empty() {
                    None
                } else {
                    Some((s, items))
                }
            })
            .collect::<Vec<(Status, Vec<Task>)>>();
    }
    println!("New time: {:?}", start2.elapsed());
}
