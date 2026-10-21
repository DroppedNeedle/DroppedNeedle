<script lang="ts">
	import { ChevronDown, FolderPlus, Plus, Trash2 } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { createUuid } from '$lib/utils/uuid';
	import type {
		LibraryIdentificationPolicy,
		LibraryPolicyTreeNode,
		LibraryRootSettings
	} from '$lib/queries/library/LibraryOperationsTypes';

	interface Props {
		roots: LibraryRootSettings[];
		onchange: (roots: LibraryRootSettings[]) => void;
	}

	let { roots, onchange }: Props = $props();
	const treeQuery = getLibraryPolicyTreeQuery();
	let rootDialog: HTMLDialogElement;
	let ruleDialog: HTMLDialogElement;
	let rootHeading: HTMLHeadingElement;
	let ruleHeading: HTMLHeadingElement;
	let rootOpener: HTMLElement | null = null;
	let ruleOpener: HTMLElement | null = null;
	let expanded = $state<string[]>([]);
	let ruleRootId = $state<string | null>(null);
	let newRootPath = $state('');
	let newRootLabel = $state('');
	let newRootPolicy = $state<LibraryIdentificationPolicy>('automatic');
	let newRulePath = $state('');
	let newRulePolicy = $state<LibraryIdentificationPolicy>('automatic');

	const policyCopy: Record<LibraryIdentificationPolicy, string> = {
		local_metadata:
			'Use file tags and embedded IDs. Do not search external metadata automatically. Library Management leaves these files alone; the tag editor still works.',
		automatic: 'Index files first, then try to identify albums in the background.',
		excluded:
			'Keep files on disk but hide this path from DroppedNeedle and connected music clients.'
	};
	const POLICY_OPTIONS: { value: LibraryIdentificationPolicy; label: string }[] = [
		{ value: 'local_metadata', label: 'Local metadata' },
		{ value: 'automatic', label: 'Automatic identification' },
		{ value: 'excluded', label: 'Excluded' }
	];

	function updateRoot(
		id: string,
		update: (root: LibraryRootSettings) => LibraryRootSettings
	): void {
		onchange(roots.map((root) => (root.id === id ? update(root) : root)));
	}

	function addRoot(): void {
		if (!newRootPath.trim() || !newRootLabel.trim()) return;
		onchange([
			...roots,
			{
				id: createUuid(),
				path: newRootPath.trim(),
				label: newRootLabel.trim(),
				policy: newRootPolicy,
				rules: []
			}
		]);
		newRootPath = '';
		newRootLabel = '';
		newRootPolicy = 'automatic';
		rootDialog.close();
	}

	function addRule(): void {
		if (!ruleRootId || !newRulePath.trim()) return;
		updateRoot(ruleRootId, (root) => ({
			...root,
			rules: [
				...root.rules,
				{ id: createUuid(), relative_path: newRulePath.trim(), policy: newRulePolicy }
			]
		}));
		newRulePath = '';
		newRulePolicy = 'automatic';
		ruleDialog.close();
	}

	function openRoot(event: MouseEvent & { currentTarget: HTMLElement }): void {
		rootOpener = event.currentTarget;
		rootDialog.showModal();
		rootHeading.focus();
	}

	function openRule(rootId: string, event: MouseEvent & { currentTarget: HTMLElement }): void {
		ruleOpener = event.currentTarget;
		ruleRootId = rootId;
		ruleDialog.showModal();
		ruleHeading.focus();
	}

	function policyRows(
		root: LibraryRootSettings,
		treeRoot: LibraryPolicyTreeNode | undefined
	): LibraryPolicyTreeNode[] {
		const currentRuleIds = new Set(root.rules.map((rule) => rule.id));
		const rows = (treeRoot?.children ?? []).filter((row) => currentRuleIds.has(row.id));
		const returnedIds = new Set(rows.map((row) => row.id));
		for (const rule of root.rules) {
			if (returnedIds.has(rule.id)) continue;
			rows.push({
				id: rule.id,
				kind: 'rule',
				label: rule.relative_path.split('/').at(-1) ?? rule.relative_path,
				path: rule.relative_path,
				policy: rule.policy,
				inherited_from_id: rule.id,
				available: true,
				indexed_file_count: null,
				on_disk_file_count: null,
				children: []
			});
		}
		return rows.sort((left, right) => left.path.localeCompare(right.path));
	}

	function inheritanceSource(node: LibraryPolicyTreeNode, root: LibraryRootSettings): string {
		if (node.inherited_from_id === node.id) return 'Explicit override';
		if (node.inherited_from_id === root.id) return `Inherited from ${root.label}`;
		const source = root.rules.find((rule) => rule.id === node.inherited_from_id);
		return source ? `Inherited from ${source.relative_path}` : 'Inherited policy';
	}
</script>

<section class="space-y-3" aria-labelledby="library-roots-title">
	<div class="flex flex-wrap items-center justify-between gap-3">
		<div>
			<h3 id="library-roots-title" class="font-semibold">Library roots</h3>
			<p class="text-xs text-base-content/55">
				Set the default identification policy for each root, then add directory overrides only where
				needed.
			</p>
		</div>
		<Button variant="outline" size="sm" icon={FolderPlus} onclick={openRoot}>Add root</Button>
	</div>

	{#if roots.length === 0}
		<div
			class="rounded-box border border-dashed border-base-content/20 p-6 text-center text-sm text-base-content/55"
		>
			No library roots configured.
		</div>
	{/if}
	{#each roots as root (root.id)}
		{@const treeRoot = treeQuery.data?.roots.find((node) => node.id === root.id)}
		{@const effectiveRows = policyRows(root, treeRoot)}
		<Card as="article" variant="bordered" padding="none" class="overflow-hidden">
			<div class="grid gap-3 p-4 lg:grid-cols-[minmax(0,1fr)_14rem_auto] lg:items-start">
				<div class="min-w-0">
					<div class="flex flex-wrap items-center gap-2">
						<h4 class="font-semibold">{root.label}</h4>
						{#if treeRoot && !treeRoot.available}<span class="badge badge-warning badge-sm"
								>Unavailable</span
							>{/if}
					</div>
					<p class="truncate font-mono text-xs text-base-content/55" title={root.path}>
						{root.path}
					</p>
					<p class="mt-1 text-xs text-base-content/45">
						{treeRoot?.indexed_file_count?.toLocaleString() ?? '-'} indexed · {treeRoot?.on_disk_file_count?.toLocaleString() ??
							'-'} on disk
					</p>
				</div>
				<label class="grid gap-1 text-xs">
					<span>Default policy</span>
					<select
						class="select select-sm"
						value={root.policy}
						onchange={(event) =>
							updateRoot(root.id, (value) => ({
								...value,
								policy: event.currentTarget.value as LibraryIdentificationPolicy
							}))}
					>
						<option value="local_metadata">Local metadata</option><option value="automatic"
							>Automatic identification</option
						><option value="excluded">Excluded</option>
					</select>
					<span class="mt-1 text-xs text-base-content/50">{policyCopy[root.policy]}</span>
				</label>
				<div class="flex gap-1 lg:justify-end">
					<Button
						variant="ghost"
						size="sm"
						onclick={() =>
							(expanded = expanded.includes(root.id)
								? expanded.filter((id) => id !== root.id)
								: [...expanded, root.id])}
						aria-expanded={expanded.includes(root.id)}
						><ChevronDown
							class={`h-4 w-4 transition-transform ${expanded.includes(root.id) ? 'rotate-180' : ''}`}
						/> Rules</Button
					>
					<IconButton
						variant="danger-ghost"
						size="sm"
						shape="square"
						icon={Trash2}
						label={`Remove ${root.label}`}
						onclick={() => onchange(roots.filter((value) => value.id !== root.id))}
					/>
				</div>
			</div>
			{#if expanded.includes(root.id)}
				<div class="border-t border-base-content/10 p-3">
					<div class="mb-2 flex items-center justify-between">
						<h5 class="text-sm font-semibold">Directory overrides</h5>
						<Button
							variant="ghost"
							size="xs"
							icon={Plus}
							onclick={(event) => openRule(root.id, event)}>Add override</Button
						>
					</div>
					{#if effectiveRows.length === 0}<p class="text-xs text-base-content/50">
							Every directory inherits {root.policy.replace('_', ' ')}.
						</p>{/if}
					<div class="space-y-1">
						{#each effectiveRows as node (node.id)}
							{@const rule = root.rules.find((value) => value.id === node.id)}
							<div
								class="grid gap-2 rounded-lg bg-base-100 px-3 py-2 text-sm sm:grid-cols-[minmax(0,1fr)_12rem_auto] sm:items-center"
							>
								<div class="min-w-0">
									<p class="truncate font-mono text-xs" title={node.path}>{node.path}</p>
									<p class="mt-0.5 text-xs text-base-content/50">
										{inheritanceSource(node, root)} · {node.indexed_file_count?.toLocaleString() ??
											'-'}
										indexed{#if !node.available}
											· Path not currently found{/if}
									</p>
								</div>
								{#if rule}
									<select
										class="select select-sm"
										value={rule.policy}
										aria-label={`Policy for ${rule.relative_path}`}
										onchange={(event) =>
											updateRoot(root.id, (value) => ({
												...value,
												rules: value.rules.map((current) =>
													current.id === rule.id
														? {
																...current,
																policy: event.currentTarget.value as LibraryIdentificationPolicy
															}
														: current
												)
											}))}
										><option value="local_metadata">Local metadata</option><option value="automatic"
											>Automatic</option
										><option value="excluded">Excluded</option></select
									>
									<IconButton
										size="xs"
										shape="square"
										icon={Trash2}
										label={`Remove override ${rule.relative_path}`}
										onclick={() =>
											updateRoot(root.id, (value) => ({
												...value,
												rules: value.rules.filter((current) => current.id !== rule.id)
											}))}
									/>
								{:else}
									<span class="badge badge-outline">{node.policy.replace('_', ' ')}</span>
									<span aria-hidden="true"></span>
								{/if}
							</div>
						{/each}
					</div>
				</div>
			{/if}
		</Card>
	{/each}
</section>

<dialog
	bind:this={rootDialog}
	class="modal"
	aria-labelledby="add-root-title"
	onclose={() => rootOpener?.focus()}
>
	<div class="modal-box max-w-xl overflow-hidden p-0">
		<header class="border-b border-base-content/10 bg-base-200/70 px-6 py-5">
			<div class="flex items-start gap-3">
				<div class="rounded-xl bg-primary/12 p-2.5 text-primary">
					<FolderPlus class="h-5 w-5" aria-hidden="true" />
				</div>
				<div>
					<h2 bind:this={rootHeading} id="add-root-title" tabindex="-1" class="text-lg font-bold">
						Add library root
					</h2>
					<p class="mt-1 text-sm text-base-content/60">
						Choose the folder DroppedNeedle should index and how new albums should be identified.
					</p>
				</div>
			</div>
		</header>
		<form
			onsubmit={(event) => {
				event.preventDefault();
				addRoot();
			}}
		>
			<div class="space-y-5 px-6 py-5">
				<TextField
					label="Name"
					bind:value={newRootLabel}
					placeholder="Main library"
					help="A short name shown in Library settings."
				/>
				<TextField
					label="Folder path"
					mono
					bind:value={newRootPath}
					placeholder="/music"
					help="Use the path as DroppedNeedle sees it inside the container."
				/>
				<div class="grid gap-2">
					<SelectField
						label="Default identification"
						bind:value={newRootPolicy}
						options={POLICY_OPTIONS}
					/>
					<span class="rounded-box bg-base-200/70 px-3 py-2 text-xs text-base-content/60">
						{policyCopy[newRootPolicy]}
					</span>
				</div>
			</div>
			<div class="modal-action mt-0 border-t border-base-content/10 bg-base-200/40 px-6 py-4">
				<Button variant="ghost" onclick={() => rootDialog.close()}>Cancel</Button>
				<Button
					type="submit"
					variant="primary"
					disabled={!newRootLabel.trim() || !newRootPath.trim()}>Add root</Button
				>
			</div>
		</form>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close add root dialog">close</button>
	</form>
</dialog>

<dialog
	bind:this={ruleDialog}
	class="modal"
	aria-labelledby="add-rule-title"
	onclose={() => ruleOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2 bind:this={ruleHeading} id="add-rule-title" tabindex="-1" class="text-lg font-bold">
			Add directory override
		</h2>
		<p class="mt-2 text-sm text-base-content/60">
			Enter a path relative to this library root. Globs and regular expressions are not used.
		</p>
		<div class="mt-4 space-y-3">
			<TextField
				label="Relative path"
				mono
				bind:value={newRulePath}
				placeholder="Compilations/Live"
			/>
			<SelectField
				label="Policy"
				bind:value={newRulePolicy}
				options={POLICY_OPTIONS}
				help={policyCopy[newRulePolicy]}
			/>
		</div>
		<div class="modal-action">
			<Button variant="ghost" onclick={() => ruleDialog.close()}>Cancel</Button><Button
				variant="primary"
				disabled={!newRulePath.trim()}
				onclick={addRule}>Add override</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close add override dialog">close</button>
	</form>
</dialog>
