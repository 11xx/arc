-- | Post-integration audits.
--
-- An audit is anchored to a revision that shipped, so it can never rewrite
-- the answer to what shipped with what review. A negative audit may fulfil a
-- coverage obligation: a read happened, and the problems it raised stay
-- open. Fulfilled is not approved, and the audit's verdict is its own field.
module Arc.Model.Audit
  ( AuditRefusal (..)
  , auditRefusalText
  , Discharge (..)
  , auditDischarges
  , auditIsIndependent
  ) where

import Arc.Model.Debt (ReadEvidence (..), auditIsIndependent, debtKindFor)
import Arc.Model.History
import Arc.Model.Identifiers

data AuditRefusal
  = AuditWhileOpen
  | AuditAssumedAuditor
  | AuditAuditorNotIndependent
  deriving (Eq, Ord, Show)

auditRefusalText :: AuditRefusal -> String
auditRefusalText refusal = case refusal of
  AuditWhileOpen -> "the change is open; an audit reviews a revision that shipped"
  AuditAssumedAuditor -> "an approving audit needs a declared identity, not an assumed one"
  AuditAuditorNotIndependent -> "an approving audit must come from another identity"

-- | What an audit discharged, and what it left open. 'dischargeApproves' is
-- derived from the audit alone and never from the fact that the read was
-- fulfilled.
data Discharge = Discharge
  { dischargeDebt :: DebtId
  , dischargeKind :: DebtKind
  , dischargeRead :: ReadEvidence
  , dischargeVerdict :: VerdictKind
  , dischargeApproves :: Bool
  , dischargeOpenFindings :: [FindingId]
  }
  deriving (Eq, Ord, Show)

-- | Decide what an audit does to a recorded obligation. The state must
-- already include the audit event and any findings it raised.
auditDischarges :: ChangeState -> Debt -> Audit -> Either AuditRefusal Discharge
auditDischarges state debt audit = case latestIntegration state of
  Nothing -> Left AuditWhileOpen
  Just _ -> case approvingRefusal of
    Just refusal -> Left refusal
    Nothing ->
      Right
        Discharge
          { dischargeDebt = debtId debt
          , dischargeKind = debtKindFor state debt
          , dischargeRead = ReadByAudit (auditEvent audit)
          , dischargeVerdict = auditKind audit
          , dischargeApproves = auditKind audit == Approved
          , dischargeOpenFindings = openAuditFindings state
          }
  where
    approvingRefusal
      | auditKind audit /= Approved = Nothing
      | auditAssumed audit = Just AuditAssumedAuditor
      | not (auditIsIndependent state audit) = Just AuditAuditorNotIndependent
      | otherwise = Nothing
