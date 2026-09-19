-- | The tiny test harness and its rendering helpers.
module Render
  ( Check (..)
  , passCheck
  , failCheck
  , expectEq
  , expectTrue
  , expectPermittedWith
  , expectRefusedWith
  , counterexampleText
  ) where

import Arc.Model

data Check = Check
  { checkName :: String
  , checkPassed :: Bool
  , checkDetail :: String
  }

passCheck :: String -> String -> Check
passCheck name detail = Check {checkName = name, checkPassed = True, checkDetail = detail}

failCheck :: String -> String -> Check
failCheck name detail = Check {checkName = name, checkPassed = False, checkDetail = detail}

expectEq :: (Eq a, Show a) => String -> a -> a -> Check
expectEq name expected actual
  | expected == actual = passCheck name ""
  | otherwise =
      failCheck name ("expected " <> oneLine (show expected) <> ", got " <> oneLine (show actual))

expectTrue :: String -> String -> Bool -> Check
expectTrue name detail condition
  | condition = passCheck name ""
  | otherwise = failCheck name detail

expectPermittedWith :: String -> Authorization -> Decision -> Check
expectPermittedWith name expected decision = case decision of
  Permitted basis
    | basisAuthorization basis == expected -> passCheck name ""
    | otherwise ->
        failCheck name ("permitted on " <> oneLine (show (basisAuthorization basis)) <> ", expected " <> oneLine (show expected))
  Refused refusal -> failCheck name ("refused: " <> refusalText refusal)

expectRefusedWith :: String -> String -> Decision -> Check
expectRefusedWith name expectedTag decision = case decision of
  Refused refusal
    | refusalTag refusal == expectedTag -> passCheck name (refusalTag refusal)
    | otherwise -> failCheck name ("refused as " <> refusalTag refusal <> ", expected " <> expectedTag)
  Permitted basis -> failCheck name ("permitted on " <> oneLine (show (basisAuthorization basis)))

counterexampleText :: String -> String
counterexampleText = take 1600 . oneLine

oneLine :: String -> String
oneLine = unwords . words
