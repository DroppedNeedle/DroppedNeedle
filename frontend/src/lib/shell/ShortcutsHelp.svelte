<script lang="ts">
	/*
	 * ShortcutsHelp — the "?" keyboard-shortcuts overlay.
	 *
	 * Mounted inside CommandPalette (always rendered in the shell) so it needs no
	 * layout wiring. Lists only shortcuts that actually exist: the palette keys
	 * (CommandPalette.svelte) and the global player keys (+layout.svelte
	 * handleGlobalKeydown, active only while the player is visible).
	 */

	let dialogEl: HTMLDialogElement | undefined = $state();

	type ShortcutRow = { keys: string[]; label: string };
	type ShortcutGroup = { label: string; rows: ShortcutRow[] };

	const groups: ShortcutGroup[] = [
		{
			label: 'Anywhere',
			rows: [
				{ keys: ['Ctrl', 'K'], label: 'Open / close palette' },
				{ keys: ['/'], label: 'Open palette' },
				{ keys: ['?'], label: 'Show shortcuts' },
				{ keys: ['Esc'], label: 'Close dialog' }
			]
		},
		{
			label: 'Palette',
			rows: [
				{ keys: ['↑', '↓'], label: 'Navigate results' },
				{ keys: ['↵'], label: 'Select / search' }
			]
		},
		{
			label: 'Player · when visible',
			rows: [
				{ keys: ['Space'], label: 'Play / pause' },
				{ keys: ['←', '→'], label: 'Seek ±10s' },
				{ keys: ['↑', '↓'], label: 'Volume' }
			]
		}
	];

	function handleGlobalKeydown(e: KeyboardEvent) {
		if (e.key !== '?') return;
		const target = e.target as HTMLElement;
		const inField =
			target?.tagName === 'INPUT' ||
			target?.tagName === 'TEXTAREA' ||
			target?.tagName === 'SELECT' ||
			target?.isContentEditable;
		if (inField) return;
		if (document.querySelector('dialog[open]')) return;
		e.preventDefault();
		dialogEl?.showModal();
	}
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<dialog class="modal" bind:this={dialogEl}>
	<div class="dn-shortcuts modal-box max-w-xl overflow-hidden p-0">
		<div class="flex items-center gap-4 border-b border-base-content/10 px-6 py-4">
			<h2 class="font-display text-lg font-medium">Keyboard shortcuts</h2>
			<span class="dn-shortcuts__esc ml-auto hidden sm:inline-flex">Esc to close</span>
		</div>

		<div class="grid gap-x-10 gap-y-6 px-6 py-5 sm:grid-cols-2">
			{#each groups as group (group.label)}
				<section>
					<p
						class="mb-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						{group.label}
					</p>
					{#each group.rows as row (row.keys.join('-') + row.label)}
						<div class="flex items-center justify-between gap-4 py-1.5">
							<span class="text-sm text-base-content/80">{row.label}</span>
							<span class="flex shrink-0 items-center gap-1">
								{#each row.keys as key (key)}
									<kbd class="kbd kbd-sm">{key}</kbd>
								{/each}
							</span>
						</div>
					{/each}
				</section>
			{/each}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close">close</button>
	</form>
</dialog>

<style>
	/* same surface treatment as the command palette */
	.dn-shortcuts {
		border: 1px solid var(--dn-hairline);
		box-shadow: var(--dn-shadow-4);
		background: oklch(from var(--color-base-200) l c h / 0.97);
		backdrop-filter: blur(24px) saturate(1.05);
		-webkit-backdrop-filter: blur(24px) saturate(1.05);
	}

	.dn-shortcuts__esc {
		align-items: center;
		padding: 0.28rem 0.65rem;
		border-radius: 0.5rem;
		border: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-content) l c h / 0.05);
		font-family: var(--font-mono);
		font-size: 0.58rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}
</style>
