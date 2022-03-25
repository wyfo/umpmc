# umpmc

Unbounded Multiple Producer Multiple Consumer queue (lock free, obviously)

This algorithm is a POC. It's implemented as a doubly linked list, with a node cache. I've spent my time into writing
this README instead of code comments, but will maybe find the time to add them later.

## Brief explanation

Nodes are inserted atomically in a LIFO way on the queue *head*, with their *prev* field pointing to the previous *head*
; *next* pointer of the previous head (or queue's *tail* pointer when no previous head) is set atomically in a third
step. The second step is to set a unique *index* to the node, which is simply the index of the previous node plus one;
if the previous node index is not set, *prev* chain is scanned until a set index is find (with another plus one for each
node scanned). If the tail node doesn't have an *index*, then the current queue *index* is used; then the tail node
index is checked again, so the index is guaranteed to be the correct one (if the queue index was outdated, then the tail
index should be updated).

To ensure dequeue uniqueness, queue has an *index* which is incremented atomically when a node is about to be dequeued;
it must match node's *index*. Because of concurrent assignments, queue's *tail* is not guaranteed to be the exact tail
of the queue. That's why when the *tail* node index doesn't match, queue is scanned using *next* until a node with
matching index is found, or the dequeue is rejected. There are two cases:

- The *tail* is the *head*, i.e. there is only one node, then the node is taken out of the queue atomically by setting
  the *head* ptr. When it fails, i.e. a second node has been inserted concurrently, it goes to the second case.
- There is more than one node (*tail* is different from *head*), in that case, *tail* node's *next* pointer **MUST** be
  set, or the dequeue is rejected; it guarantees that *tail*'s *index* will be set (as prev *next* is set **after**
  *index*) and prevent concurrent set of *next*. If the dequeue is rejected, but queue's *index* has already been
  incremented (coming from the first case), then the *index* is decremented; decrement failure means that another node
  is being dequeued, which means that node *next* has concurrently been set (because the other node has been found by
  scanning from *next*), which means that the dequeue can continue. Then the queue's *tail* is reassigned with
  compare-and-swap, but it can fail; the operation is retried when the tail concurrently set is not a guaranteed tail (
  node's *index* equal to the queue *
  index* minus one, i.e. last dequeue, or new tail with *index* equal to queue index, e.g. enqueue after dequeue of the
  last node). Finally, the node is invalidated, i.e. *index* is unset and *next* set to null.

Allocated nodes are cached using an atomic LIFO stack. If it prevents unnecessary allocations, it mainly prevents nodes
to be freed and having dangling *tail* and *next*.

## Analysis

The algorithm seems to work :) Contrary to the
famous [Dmitry Vyukov's algorithm](https://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue) (which
was previously part of Rust standard library), it's unbounded. Actually, it could be easy to add a bound, by
preallocating the cache nodes and preventing further allocations, but this algorithm would surely perform worse than the
other one.

One of the edge cases is the constraint of having node's *next* set to dequeue it. It can lead to situation where there
is for example one node fully inserted in the queue, and another one currently being added. In this case, dequeueing the
first node will fail as long as the second insertion is not finished. However, dequeue has a special
return `Dequeue::Spin` in this case, to indicate that a spin loop can be used to get a successful dequeue; `dequeue_spin` method can also be used to do the spin loop directly during the dequeue process.

Because the cache ensure that there is no dangling pointers, it cannot be shrunk.

Some optimization are possible, for example concerning memory ordering of atomic operations. I'm not an expert in this
domain, and I did not think too much about it. Cache-padding is an obvious optimization for atomic fields, using for
example `cache-padded` library, but I did not add it as it's not necessary for the algorithm.

## Why?

Unbounded queue is not always a good idea, and there is a bounded algorithm more optimized. Here is the reason why I've
designed this algorithm.

I wanted to go back to Rust, and I was looking for a project idea. I was thinking about Go channels, wondering why there
was no (more) standard MPMC in Rust, and had the idea to design one using two atomic indexes (yes, like the bounded
algorithm, but I had the idea before knowing it, I swear). I did some non-convincing tries — it had been the first time
for a while that I went deep into atomic, memory ordering, etc. — and when I was finally a little bit satisfied of my
algorithm.

Then, I looked about the state of the art, and I discovered Dmitry Vyukov's algorithm. I took my time to understand it,
but I also understood that my tries were completely wrong, and that MPMC was a harder thing than I thought. (I'd also
discovered that Go channel were using locks, but they were designed by Dmitry Vyukov too, so he knew what he was doing).
So I tried to adapt my algorithm with what I'd learnt/understood, but I did not manage to find something else than the
existing bounded MPMC ...

It's frustrating to think hours about something and to realize some people smarter than you have already solved your
problem in a cleverer way, even if studying the solution is pleasant, of course. I was frustrated and really wanted to
find something myself, even if it would surely be less optimized, at least it would be original. So I've thought more
hours, about bounded, but also unbounded algorithm, using array buffer, circular lists, linked lists, etc. And I finally
found this algorithm. I did not find it on the internet (but I did not crawl all the web), so I think it's original.
Anyway, it cames from my brain, and that's cool enough for me.

