; autoscaler.lisp
;
; ACL2 model of the unified autoscaler controller.
;
; This file has three sections:
;   1. MODEL     -- fixed-point integer encoding of the Rust controller
;   2. EQUIVALENCE -- argument that the model faithfully represents the Rust code
;   3. THEOREMS  -- formal proofs of Property 1 (safety bounds) and
;                   Property 9 (monotonicity)

(in-package "ACL2")


; ═══════════════════════════════════════════════════════════════════════════════
; SECTION 1 — MODEL
; ═══════════════════════════════════════════════════════════════════════════════


(defconst *min-replicas*  1)
(defconst *max-replicas*  20)
(defconst *min-cpu*       5)   ; 0.5 * 10
(defconst *max-cpu*       40)  ; 4.0 * 10
(defconst *min-mem*       10)  ; 1.0 * 10
(defconst *max-mem*       80)  ; 8.0 * 10
(defconst *slots*         32)  ; slots_per_replica
(defconst *cpu-hi*        70)  ; cpu_util_target (%)
(defconst *mem-hi*        70)  ; mem_util_target (%)
(defconst *cpu-lo*        21)  ; cpu_util_target * 0.3 = very_low threshold
(defconst *mem-lo*        21)  ; mem_util_target * 0.3

; ── Helper: clamp ─────────────────────────────────────────────────────────────
;
; Rust:  x.clamp(lo, hi)  ≡  x.max(lo).min(hi)
; This is the final guard applied to every output of scale().

(defun ac-clamp (x lo hi)
  (declare (xargs :guard (and (integerp x)
                              (integerp lo)
                              (integerp hi)
                              (<= lo hi))))
  (if (< x lo)
      lo
    (if (> x hi)
        hi
      x)))

; ── Helper: ceiling division ──────────────────────────────────────────────────

(defun ceil-div (a b)
  (declare (xargs :guard (and (natp a) (posp b))
                  :measure (nfix a)))
  (cond ((zp a)   0)
        ((zp b)   1)       ; guard violation branch — makes b>0 visible to termination checker
        ((< a b)  1)
        (t        (+ 1 (ceil-div (- a b) b)))))

; ── Helper: floor division ────────────────────────────────────────────────────

(defun floor-div (n d)
  (declare (xargs :guard (and (natp n) (posp d))
                  :measure (nfix n)))
  (cond ((zp n)   0)
        ((zp d)   0)       ; guard violation branch
        ((< n d)  0)
        (t        (+ 1 (floor-div (- n d) d)))))

; ── active-slots ──────────────────────────────────────────────────────────────

(defun active-slots (cpu-util total-slots)
  (declare (xargs :guard (and (natp cpu-util) (natp total-slots))))
  (floor-div (* cpu-util total-slots) 100))

; ── replicas-for-demand ───────────────────────────────────────────────────────

(defun replicas-for-demand (queue cpu-util num-replicas)
  (declare (xargs :guard (and (natp queue)
                              (natp cpu-util)
                              (posp num-replicas))))
  (let* ((total-slots (* num-replicas *slots*))
         (active      (active-slots cpu-util total-slots))
         (demand      (+ queue active)))
    (if (= demand 0)
        num-replicas
      (let* ((needed  (ceil-div demand *slots*))
             (bounded (min needed (* 3 num-replicas)))
             (result  (max bounded num-replicas)))
        (max result 1)))))

; ── diagnose ──────────────────────────────────────────────────────────────────

(defun ac-diagnose (queue cpu-util mem-util num-replicas lat)
  (declare (xargs :guard (and (natp queue)
                              (natp cpu-util)
                              (natp mem-util)
                              (posp num-replicas)
                              (booleanp lat))))
  (let ((has-queue (> queue 0))
        (high-cpu  (> cpu-util *cpu-hi*))
        (high-mem  (> mem-util *mem-hi*))
        (very-low  (and (< cpu-util *cpu-lo*)
                        (< mem-util *mem-lo*))))
    (cond
      ; latency_under_load || has_queue → diagnose type of load
      ((and (or lat has-queue) high-cpu high-mem) 'mixed)
      ((and (or lat has-queue) high-cpu)          'cpu)
      ((and (or lat has-queue) high-mem)          'memory)
      ((or lat has-queue)                          'load)
      ; very_low_cpu && very_low_mem && no_queue && above min
      ((and very-low (> num-replicas *min-replicas*)) 'overprovisioned)
      ; everything else is stable or unknown — no action
      (t                                           'stable))))

; ── scale ─────────────────────────────────────────────────────────────────────

(defun ac-scale (queue cpu-util mem-util num-replicas cpu-tenths mem-tenths bottleneck)
  (declare (xargs :guard (and (natp queue)
                              (natp cpu-util)
                              (natp mem-util)
                              (posp num-replicas)
                              (posp cpu-tenths)
                              (posp mem-tenths)
                              (symbolp bottleneck))))
  (let* (
    ; Compute raw (pre-clamp) values for each bottleneck type.
    ; Each arm mirrors the corresponding match arm in scale().

    (demand-r (replicas-for-demand queue cpu-util num-replicas))

    (raw-r
      (case bottleneck
        ; Bottleneck::Load — scale out to meet demand
        (load  demand-r)

        ; Bottleneck::Cpu — scale CPU up by 1.3x; fall back to horizontal if
        ; vertical is saturated or demand already exceeds current replicas
        (cpu   (let ((new-cpu (floor-div (* cpu-tenths 13) 10)))
                 (if (or (> demand-r num-replicas)
                         (>= new-cpu *max-cpu*))
                     demand-r
                   num-replicas)))

        ; Bottleneck::Memory — same logic as Cpu but for memory
        (memory (let ((new-mem (floor-div (* mem-tenths 13) 10)))
                  (if (or (> demand-r num-replicas)
                          (>= new-mem *max-mem*))
                      demand-r
                    num-replicas)))

        ; Bottleneck::Mixed — both axes + horizontal
        (mixed  demand-r)

        ; Bottleneck::Overprovisioned — step down gradually
        ; Rust: if r <= 5 then r-1 else floor(r * 0.8).max(min_replicas)
        (overprovisioned
          (if (<= num-replicas 5)
              (if (> num-replicas *min-replicas*)
                  (- num-replicas 1)
                num-replicas)
            (max (floor-div (* num-replicas 8) 10) *min-replicas*)))

        ; Bottleneck::Stable / Unknown — no change
        (otherwise  num-replicas)))

    (raw-cpu
      (case bottleneck
        (cpu           (floor-div (* cpu-tenths 13) 10))
        (mixed         (floor-div (* cpu-tenths 12) 10))
        (overprovisioned
          (if (< cpu-util 20)
              (max (floor-div (* cpu-tenths 9) 10) *min-cpu*)
            cpu-tenths))
        (otherwise     cpu-tenths)))

    (raw-mem
      (case bottleneck
        (memory        (floor-div (* mem-tenths 13) 10))
        (mixed         (floor-div (* mem-tenths 12) 10))
        (overprovisioned
          (if (< mem-util 20)
              (max (floor-div (* mem-tenths 9) 10) *min-mem*)
            mem-tenths))
        (otherwise     mem-tenths))))

    ; ── The clamp is always applied last ─────────────────────────────────────
    ; This is the single enforcement point for Property 1.
    (list (ac-clamp raw-r    *min-replicas* *max-replicas*)
          (ac-clamp raw-cpu  *min-cpu*      *max-cpu*)
          (ac-clamp raw-mem  *min-mem*      *max-mem*))))

; ── decide ────────────────────────────────────────────────────────────────────

(defun ac-decide (queue cpu-util mem-util num-replicas cpu-tenths mem-tenths lat)
  (declare (xargs :guard (and (natp queue)
                              (natp cpu-util)
                              (natp mem-util)
                              (posp num-replicas)
                              (posp cpu-tenths)
                              (posp mem-tenths)
                              (booleanp lat))))
  (ac-scale queue cpu-util mem-util num-replicas cpu-tenths mem-tenths
            (ac-diagnose queue cpu-util mem-util num-replicas lat)))


; ═══════════════════════════════════════════════════════════════════════════════
; SECTION 2 — EQUIVALENCE ARGUMENT
; ═══════════════════════════════════════════════════════════════════════════════
;
; ACL2 cannot execute Rust code directly.  Equivalence is argued in three parts:
;
; (a) STRUCTURAL CORRESPONDENCE
;     Each ACL2 function mirrors its Rust counterpart branch-for-branch:
;
;       Rust                            ACL2
;       x.clamp(lo, hi)                 (ac-clamp x lo hi)
;       (a as f64 / b as f64).ceil()    (ceil-div a b)
;       cpu_util / 100.0 * slots        (floor (* cpu-util slots) 100)
;       replicas_for_demand()           (replicas-for-demand q u r)
;       diagnose()                      (ac-diagnose q u m r lat)
;       scale()                         (ac-scale ...)
;       decide()                        (ac-decide ...)
;
;     The match arms in scale() map one-to-one to the case branches in ac-scale.
;     The if/else logic in diagnose() maps one-to-one to the cond in ac-diagnose.
;
; (b) ENCODING CORRECTNESS
;     For any Rust float v ∈ {cpu_per_replica, mem_per_replica}:
;       |encode(v) / 10 - v| < 0.1
;     This follows from the definition encode(v) = round(v * 10), which has
;     rounding error < 0.05 < 0.1.
;
;     The scale multipliers are encoded as:
;       1.3 → 13/10  (error = 0.0,  exact)
;       1.2 → 12/10  (error = 0.0,  exact)
;       0.9 →  9/10  (error = 0.0,  exact)
;       0.8 →  8/10  (error = 0.0,  exact)
;     So multiplier encoding is exact; no approximation error there.
;
;     The only approximation is floor vs. round for active-slots.  Floor
;     under-estimates active slots by at most 1, which means the model
;     may request slightly fewer replicas than the Rust code would.  This
;     makes the safety proof conservative: if ACL2 proves bounds hold,
;     the Rust code (which may request >= what ACL2 models) will also
;     stay within those bounds after the final clamp.
;
; (c) CLAMP IS THE SINGLE ENFORCEMENT POINT
;     Both the Rust and ACL2 models apply the resource bounds exactly once,
;     as the last operation before returning.  Any divergence in intermediate
;     arithmetic cannot escape the clamp.  Therefore:
;
;       Theorem (informal): for any input, ac-decide returns values in the
;       same [lo, hi] intervals as the Rust decide() function, regardless
;       of intermediate computation differences.
;
;     This is a direct consequence of Theorem safety-replicas / safety-cpu /
;     safety-mem below, which prove the ACL2 model is bounded, plus the
;     structural argument that the Rust clamp is identical.


; ═══════════════════════════════════════════════════════════════════════════════
; SECTION 3 — THEOREMS
; ═══════════════════════════════════════════════════════════════════════════════

(defthm clamp-lower-bound
  (implies (and (integerp x)
                (integerp lo)
                (integerp hi)
                (<= lo hi))
           (<= lo (ac-clamp x lo hi)))
  :hints (("Goal" :in-theory (enable ac-clamp))))

(defthm clamp-upper-bound
  (implies (and (integerp x)
                (integerp lo)
                (integerp hi)
                (<= lo hi))
           (<= (ac-clamp x lo hi) hi))
  :hints (("Goal" :in-theory (enable ac-clamp))))

(defthm clamp-in-bounds
  (implies (and (integerp x)
                (integerp lo)
                (integerp hi)
                (<= lo hi))
           (and (<= lo (ac-clamp x lo hi))
                (<= (ac-clamp x lo hi) hi)))
  :hints (("Goal" :use ((:instance clamp-lower-bound)
                        (:instance clamp-upper-bound)))))

; Monotonicity of ac-clamp: if a <= b and both are clamped identically,
; the ordering is preserved.
(defthm clamp-monotone
  (implies (and (integerp a)
                (integerp b)
                (integerp lo)
                (integerp hi)
                (<= lo hi)
                (<= a b))
           (<= (ac-clamp a lo hi) (ac-clamp b lo hi)))
  :hints (("Goal" :in-theory (enable ac-clamp))))


; ── Lemmas for ceil-div ───────────────────────────────────────────────────────

(defthm ceil-div-natp
  (implies (and (natp a) (posp b))
           (natp (ceil-div a b)))
  :hints (("Goal" :induct (ceil-div a b)
           :in-theory (enable ceil-div))))

(defthm ceil-div-positive
  (implies (and (posp a) (posp b))
           (< 0 (ceil-div a b)))
  :hints (("Goal" :induct (ceil-div a b)
           :in-theory (enable ceil-div))))

(defthm ceil-div-zero-iff
  (implies (and (natp a) (posp b))
           (equal (equal (ceil-div a b) 0) (zp a)))
  :hints (("Goal" :induct (ceil-div a b) :in-theory (enable ceil-div))))

; Key lemma for monotonicity: ceil-div(a, b) <= ceil-div(a + k, b).
; Proof by induction on a (the measure of ceil-div).
(defthm ceil-div-monotone-key
  (implies (and (natp a) (natp k) (posp b))
           (<= (ceil-div a b)
               (ceil-div (+ a k) b)))
  :hints (("Goal" :induct (ceil-div a b)
           :in-theory (enable ceil-div))))

(defun ceil-div-bi-induct (a1 a2 b)
  (declare (xargs :measure (nfix a2)))
  (if (or (zp a2) (zp b) (< a2 b))
      (list a1 a2)
    (if (< a1 b)
        (list a1 a2)
      (ceil-div-bi-induct (- a1 b) (- a2 b) b))))

(defthm ceil-div-monotone
  (implies (and (natp a1) (natp a2) (posp b) (<= a1 a2))
           (<= (ceil-div a1 b) (ceil-div a2 b)))
  :hints (("Goal"
           :induct (ceil-div-bi-induct a1 a2 b)
           :in-theory (enable ceil-div))))


; ── Lemmas for floor-div ─────────────────────────────────────────────────────

; floor-div always returns a natural number.
(defthm floor-div-natp
  (implies (and (natp n) (posp d))
           (natp (floor-div n d)))
  :hints (("Goal" :induct (floor-div n d) :in-theory (enable floor-div))))

; floor-div(n, d) = 0 iff n < d (for valid inputs with n > 0).
(defthm floor-div-zero-iff
  (implies (and (natp n) (posp d))
           (equal (equal (floor-div n d) 0) (< n d)))
  :hints (("Goal" :induct (floor-div n d) :in-theory (enable floor-div))))

; Custom 2D induction scheme for floor-div monotonicity.
(defun floor-div-bi-induct (n1 n2 d)
  (declare (xargs :measure (nfix n2)))
  (if (or (zp n2) (zp d) (< n2 d))
      (list n1 n2)
    (if (< n1 d)
        (list n1 n2)
      (floor-div-bi-induct (- n1 d) (- n2 d) d))))

; floor-div is monotone non-decreasing in its first argument.
(defthm floor-div-monotone
  (implies (and (natp n1) (natp n2) (posp d) (<= n1 n2))
           (<= (floor-div n1 d) (floor-div n2 d)))
  :hints (("Goal"
           :induct (floor-div-bi-induct n1 n2 d)
           :in-theory (enable floor-div))))


; ── Lemmas for active-slots ───────────────────────────────────────────────────

; active-slots always returns a natural number.
(defthm active-slots-natp
  (implies (and (natp u) (natp s))
           (natp (active-slots u s)))
  :rule-classes (:rewrite :type-prescription)
  :hints (("Goal" :in-theory (enable active-slots)
           :use ((:instance floor-div-natp (n (* u s)) (d 100)))
           :nonlinearp t)))

; active-slots is monotone in cpu-util.
; floor-div(u1 * s, 100) <= floor-div(u2 * s, 100) when u1 <= u2 and s >= 0.
; The key step u1*s <= u2*s follows from u1 <= u2 and s >= 0 (nonlinear).
(defthm active-slots-monotone-in-util
  (implies (and (natp u1) (natp u2) (<= u1 u2) (natp total-slots))
           (<= (active-slots u1 total-slots) (active-slots u2 total-slots)))
  :hints (("Goal" :in-theory (enable active-slots)
           :use ((:instance floor-div-monotone
                  (n1 (* u1 total-slots))
                  (n2 (* u2 total-slots))
                  (d  100)))
           :nonlinearp t)))


; ── Lemmas for replicas-for-demand ───────────────────────────────────────────

(defthm rfd-ge-current
  (implies (and (natp queue)
                (natp cpu-util)
                (posp num-replicas))
           (<= num-replicas (replicas-for-demand queue cpu-util num-replicas)))
  :hints (("Goal" :in-theory (e/d (replicas-for-demand) (active-slots floor-div)))))

; Helper: compute replica count directly from aggregate demand value.
; Factoring out the demand-computation step lets us state and prove
; monotonicity at the arithmetic level, then lift it to replicas-for-demand.
(defun rfd-from-demand (demand r)
  (declare (xargs :guard (and (natp demand) (posp r))))
  (if (= demand 0)
      r
    (let* ((needed  (ceil-div demand *slots*))
           (bounded (min needed (* 3 r)))
           (result  (max bounded r)))
      (max result 1))))

; replicas-for-demand equals rfd-from-demand on the computed demand.
; Not stored as a rewrite rule (:rule-classes nil) to avoid perturbing
; subsequent proofs that rely on replicas-for-demand in the goal.
(defthm rfd-from-demand-equiv
  (equal (replicas-for-demand queue cpu-util num-replicas)
         (rfd-from-demand (+ queue (active-slots cpu-util (* num-replicas *slots*)))
                          num-replicas))
  :rule-classes nil
  :hints (("Goal" :in-theory (enable replicas-for-demand rfd-from-demand))))

; rfd-from-demand is monotone non-decreasing in its demand argument.
; Key: larger demand => larger ceil-div => larger min => larger max.
; Disable ceil-div-monotone as a rewrite rule so the :use hypothesis
; stays in the context for linear arithmetic to consume.
(defthm rfd-from-demand-monotone
  (implies (and (natp d1) (natp d2) (<= d1 d2) (posp r))
           (<= (rfd-from-demand d1 r) (rfd-from-demand d2 r)))
  :hints (("Goal"
           :in-theory (e/d (rfd-from-demand min max)
                           ((:rewrite ceil-div-monotone)))
           :use ((:instance ceil-div-monotone
                  (a1 d1) (a2 d2) (b *slots*))))))

; replicas-for-demand is monotone non-decreasing in queue depth.
; Core of Property 9: larger queue => equal or more replicas requested.
(defthm rfd-monotone-in-queue
  (implies (and (natp q1)
                (natp q2)
                (<= q1 q2)
                (natp cpu-util)
                (posp num-replicas))
           (<= (replicas-for-demand q1 cpu-util num-replicas)
               (replicas-for-demand q2 cpu-util num-replicas)))
  :hints (("Goal"
           :use ((:instance rfd-from-demand-equiv (queue q1))
                 (:instance rfd-from-demand-equiv (queue q2))
                 (:instance rfd-from-demand-monotone
                  (d1 (+ q1 (active-slots cpu-util (* num-replicas *slots*))))
                  (d2 (+ q2 (active-slots cpu-util (* num-replicas *slots*))))
                  (r num-replicas))
                 (:instance active-slots-natp
                  (u cpu-util)
                  (s (* num-replicas *slots*)))))))

; replicas-for-demand is monotone non-decreasing in cpu-util.
; Higher utilization => equal or more replicas.
(defthm rfd-monotone-in-util
  (implies (and (natp queue)
                (natp u1)
                (natp u2)
                (<= u1 u2)
                (posp num-replicas))
           (<= (replicas-for-demand queue u1 num-replicas)
               (replicas-for-demand queue u2 num-replicas)))
  :hints (("Goal"
           :use ((:instance rfd-from-demand-equiv (cpu-util u1))
                 (:instance rfd-from-demand-equiv (cpu-util u2))
                 (:instance rfd-from-demand-monotone
                  (d1 (+ queue (active-slots u1 (* num-replicas *slots*))))
                  (d2 (+ queue (active-slots u2 (* num-replicas *slots*))))
                  (r num-replicas))
                 (:instance active-slots-monotone-in-util
                  (u1 u1) (u2 u2)
                  (total-slots (* num-replicas *slots*)))
                 (:instance active-slots-natp
                  (u u1) (s (* num-replicas *slots*)))
                 (:instance active-slots-natp
                  (u u2) (s (* num-replicas *slots*)))))))


; PROPERTY 1 — SAFETY BOUNDS
;
; For all possible inputs, the controller never outputs a replica count,
; CPU allocation, or memory allocation outside the configured limits.
;
; Corresponds directly to the three .clamp() calls at the end of
; Controller::scale() in src/shared/controller.rs lines 137-139.

(defthm safety-replicas
  ; The replica count returned by ac-decide is always in [min-replicas, max-replicas].
  (implies (and (natp queue)
                (natp cpu-util)
                (natp mem-util)
                (posp num-replicas)
                (posp cpu-tenths)
                (posp mem-tenths)
                (booleanp lat))
           (let ((result (ac-decide queue cpu-util mem-util
                                    num-replicas cpu-tenths mem-tenths lat)))
             (and (<= *min-replicas* (first result))
                  (<= (first result)  *max-replicas*))))
  :hints (("Goal" :in-theory (enable ac-decide ac-scale ac-diagnose ac-clamp
                                     replicas-for-demand active-slots floor-div ceil-div))))

(defthm safety-cpu
  ; The CPU allocation returned by ac-decide is always in [min-cpu, max-cpu].
  (implies (and (natp queue)
                (natp cpu-util)
                (natp mem-util)
                (posp num-replicas)
                (posp cpu-tenths)
                (posp mem-tenths)
                (booleanp lat))
           (let ((result (ac-decide queue cpu-util mem-util
                                    num-replicas cpu-tenths mem-tenths lat)))
             (and (<= *min-cpu*  (second result))
                  (<= (second result) *max-cpu*))))
  :hints (("Goal" :in-theory (enable ac-decide ac-scale ac-diagnose ac-clamp))))

(defthm safety-mem
  ; The memory allocation returned by ac-decide is always in [min-mem, max-mem].
  (implies (and (natp queue)
                (natp cpu-util)
                (natp mem-util)
                (posp num-replicas)
                (posp cpu-tenths)
                (posp mem-tenths)
                (booleanp lat))
           (let ((result (ac-decide queue cpu-util mem-util
                                    num-replicas cpu-tenths mem-tenths lat)))
             (and (<= *min-mem*  (third result))
                  (<= (third result) *max-mem*))))
  :hints (("Goal" :in-theory (enable ac-decide ac-scale ac-diagnose ac-clamp))))


; ═══════════════════════════════════════════════════════════════════════════════
; PROPERTY 9 — MONOTONICITY
;
; Theorem A: replicas-for-demand is monotone in queue depth.
;            (already proved as rfd-monotone-in-queue above)
;
; Theorem B: The controller never scale DOWN the replica count when the
;            diagnosed bottleneck is load, cpu, memory, or mixed.
;            More concretely: under any of these bottlenecks, the output
;            replica count is >= the current replica count.
; ═══════════════════════════════════════════════════════════════════════════════

; Helper: under a load-class bottleneck, the clamped result is >= num-replicas
; (provided num-replicas <= max-replicas, so the clamp cannot reduce it further).
; When the controller diagnoses a load/cpu/memory/mixed bottleneck,
; the output replica count is >= the current replica count.
(defthm no-scale-down-under-load
  (implies (and (natp queue)
                (natp cpu-util)
                (natp mem-util)
                (posp num-replicas)
                (<= num-replicas *max-replicas*)
                (posp cpu-tenths)
                (posp mem-tenths)
                (booleanp lat)
                (member-equal (ac-diagnose queue cpu-util mem-util num-replicas lat)
                              '(load cpu memory mixed)))
           (<= num-replicas
               (first (ac-decide queue cpu-util mem-util
                                 num-replicas cpu-tenths mem-tenths lat))))
  :hints (("Goal" :in-theory (enable ac-decide ac-scale ac-diagnose ac-clamp
                                     replicas-for-demand active-slots floor-div ceil-div)
           :use ((:instance rfd-ge-current
                  (queue queue)
                  (cpu-util cpu-util)
                  (num-replicas num-replicas))))))

; Corollary: combined monotonicity — more demand (larger queue) under load
; produces equal or more replicas than less demand.
; If q1 <= q2 and both are diagnosed as load bottlenecks, then the
; replica count for q2 is >= the replica count for q1.
(defthm monotone-queue-under-load
  (implies (and (natp q1)
                (natp q2)
                (<= q1 q2)
                (natp cpu-util)
                (natp mem-util)
                (posp num-replicas)
                (<= num-replicas *max-replicas*)
                (posp cpu-tenths)
                (posp mem-tenths)
                (equal (ac-diagnose q1 cpu-util mem-util num-replicas nil) 'load)
                (equal (ac-diagnose q2 cpu-util mem-util num-replicas nil) 'load))
           (<= (first (ac-decide q1 cpu-util mem-util
                                 num-replicas cpu-tenths mem-tenths nil))
               (first (ac-decide q2 cpu-util mem-util
                                 num-replicas cpu-tenths mem-tenths nil))))
  :hints (("Goal" :in-theory (enable ac-decide ac-scale ac-diagnose ac-clamp
                                     replicas-for-demand active-slots floor-div ceil-div)
           :use ((:instance rfd-monotone-in-queue
                  (q1 q1) (q2 q2)
                  (cpu-util cpu-util)
                  (num-replicas num-replicas))
                 (:instance clamp-monotone
                  (a (replicas-for-demand q1 cpu-util num-replicas))
                  (b (replicas-for-demand q2 cpu-util num-replicas))
                  (lo *min-replicas*)
                  (hi *max-replicas*))))))
