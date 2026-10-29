# ADR-0006: UI framework (Svelte 5 + TypeScript) and design-token system

Status: **Accepted** 2026-10-29 (owner decision) · Proposed: 2026-10-29

## Decision
Svelte 5 + TypeScript in the Tauri shell. Design tokens (`06` §5) are the single source, compiled to CSS custom properties, with dark and light themes. Components are hand-built on accessible primitives; no heavyweight component kit. MapLibre GL for maps. API types are generated from the Rust schema.

## Alternatives
React (larger ecosystem, heavier runtime); SolidJS (similar to Svelte, smaller pool).

## Validation
Screen-reader smoke tests (VoiceOver, NVDA, Orca) of the chat and transfer views in Phase C, before broad UI build-out.

## Decision record (2026-10-29)
Accepted together with ADR-0001 (owner chose Rust + Tauri with a TypeScript/Svelte UI). UX decisions: launch screen = **Chat**; chat organised **per port, with an optional merged "All traffic" view**.
