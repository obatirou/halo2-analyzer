(set-info :smt-lib-version 2.6)
(set-info :category "crafted")
(set-option :produce-models true)
(set-option :incremental true)
(set-logic QF_FF)
(define-sort F () (_ FiniteField 11))
(declare-fun A-0-0 () F)
(assert ( = (ff.mul (ff.mul (as ff1 F) A-0-0) (ff.add (as ff1 F) (ff.neg A-0-0))) (as ff0 F)))
(declare-fun A-1-0 () F)
(assert ( = (ff.mul (ff.mul (as ff1 F) A-1-0) (ff.add (as ff1 F) (ff.neg A-1-0))) (as ff0 F)))
(declare-fun A-2-0 () F)
(assert ( = (ff.mul (as ff1 F) (ff.add (ff.add A-0-0 (ff.mul (as ff2 F) A-1-0)) (ff.neg A-2-0))) (as ff0 F)))
(push)
(assert (and (and ( = A-2-0 (as ff0 F))(or (not ( = A-1-0 (as ff0 F)))(not ( = A-0-0 (as ff0 F)))))))
(pop)
(assert (or (not ( = A-0-0 (as ff0 F)))(not ( = A-2-0 (as ff0 F)))(not ( = A-1-0 (as ff0 F)))))
(push)
(assert (and (and ( = A-2-0 (as ff3 F))(or (not ( = A-1-0 (as ff1 F)))(not ( = A-0-0 (as ff1 F)))))))
(pop)
(assert (or (not ( = A-0-0 (as ff1 F)))(not ( = A-2-0 (as ff3 F)))(not ( = A-1-0 (as ff1 F)))))
(push)
(assert (and (and ( = A-2-0 (as ff1 F))(or (not ( = A-1-0 (as ff0 F)))(not ( = A-0-0 (as ff1 F)))))))
(pop)
(assert (or (not ( = A-1-0 (as ff0 F)))(not ( = A-0-0 (as ff1 F)))(not ( = A-2-0 (as ff1 F)))))
(push)
(assert (and (and ( = A-2-0 (as ff2 F))(or (not ( = A-1-0 (as ff1 F)))(not ( = A-0-0 (as ff0 F)))))))
(pop)
(assert (or (not ( = A-2-0 (as ff2 F)))(not ( = A-1-0 (as ff1 F)))(not ( = A-0-0 (as ff0 F)))))
