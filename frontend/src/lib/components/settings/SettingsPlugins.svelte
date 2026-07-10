<script lang="ts">
	import {
		ChevronDown,
		CircleCheck,
		CircleX,
		FolderOpen,
		Package,
		Puzzle,
		TriangleAlert
	} from 'lucide-svelte';

	import {
		getPluginSettingsQuery,
		getPluginsQuery,
		savePluginSettingsMutation,
		testPluginMutation,
		togglePluginMutation
	} from '$lib/queries/plugins/PluginQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type {
		PluginInfo,
		PluginSettingsField,
		PluginSettingsValue,
		PluginTestResult
	} from '$lib/types';

	const pluginsQuery = getPluginsQuery();
	const toggle = togglePluginMutation();
	const save = savePluginSettingsMutation();
	const test = testPluginMutation();

	const plugins = $derived(pluginsQuery.data?.plugins ?? []);
	const apiVersion = $derived(pluginsQuery.data?.api_version ?? 1);

	let openId = $state<string | null>(null);
	let draft = $state<Record<string, PluginSettingsValue>>({});
	let revealed = $state<Record<string, boolean>>({});
	let testResults = $state<Record<string, PluginTestResult>>({});

	// settings for the expanded card only; the query re-runs when openId changes
	const settingsQuery = getPluginSettingsQuery(() => openId ?? '');
	const schema = $derived(
		openId && settingsQuery.data?.id === openId ? settingsQuery.data.schema : []
	);

	$effect(() => {
		// seed the form once the expanded plugin's stored values arrive
		if (openId && settingsQuery.data?.id === openId) {
			draft = { ...settingsQuery.data.values };
		}
	});

	function toggleOpen(plugin: PluginInfo) {
		if (!plugin.loaded) return;
		if (openId === plugin.id) {
			openId = null;
			return;
		}
		openId = plugin.id;
		draft = {};
		revealed = {};
	}

	async function setEnabled(plugin: PluginInfo, enabled: boolean) {
		try {
			await toggle.mutateAsync({ id: plugin.id, enabled });
		} catch {
			toastStore.show({
				message: `Could not ${enabled ? 'enable' : 'disable'} ${plugin.name}`,
				type: 'error'
			});
		}
	}

	async function saveDraft(plugin: PluginInfo) {
		try {
			await save.mutateAsync({ id: plugin.id, values: draft });
			toastStore.show({ message: `${plugin.name} settings saved`, type: 'success' });
		} catch {
			toastStore.show({ message: `Could not save ${plugin.name} settings`, type: 'error' });
		}
	}

	async function runTest(plugin: PluginInfo) {
		try {
			const result = await test.mutateAsync({ id: plugin.id, values: draft });
			testResults = { ...testResults, [plugin.id]: result };
		} catch {
			testResults = {
				...testResults,
				[plugin.id]: { valid: false, message: "Couldn't finish the connection test" }
			};
		}
	}

	function fieldId(pluginId: string, key: string): string {
		return `plugin-${pluginId}-${key}`;
	}

	function stringValue(key: string): string {
		const v = draft[key];
		return v === null || v === undefined ? '' : String(v);
	}

	function setNumber(key: string, raw: string) {
		draft = { ...draft, [key]: raw === '' ? null : Number(raw) };
	}

	function setSelect(field: PluginSettingsField, raw: string) {
		// preserve the option's own type (number vs string)
		const match = field.options.find((o) => String(o.value) === raw);
		draft = { ...draft, [field.key]: match ? match.value : raw };
	}

	function sourceLabel(plugin: PluginInfo): string {
		if (plugin.builtin) return 'built-in';
		return plugin.source === 'entry_point' ? 'installed package' : 'external';
	}
</script>

<section class="space-y-4">
	<header class="space-y-1">
		<h2 class="text-lg font-semibold">Plugins</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			Acquisition plugins are the sources DroppedNeedle can download music through. Soulseek and
			Usenet ship built in; drop third-party plugins into the server's <code
				class="rounded bg-base-300/60 px-1.5 py-0.5 font-mono text-xs">plugins/</code
			> folder and restart to add more.
		</p>
	</header>

	{#if pluginsQuery.isLoading}
		<div class="flex justify-center rounded-2xl border border-base-content/8 bg-base-200/50 p-10">
			<span class="loading loading-spinner loading-md text-base-content/40"></span>
		</div>
	{:else if plugins.length === 0}
		<div
			class="flex flex-col items-center rounded-box border border-dashed border-base-300 bg-base-200/40 p-10 text-center"
		>
			<div class="grid size-14 place-items-center rounded-2xl bg-base-300/60">
				<Puzzle class="size-7 text-accent" aria-hidden="true" />
			</div>
			<p class="mt-4 font-semibold">No plugins found</p>
			<p class="mx-auto mt-1 max-w-md text-sm text-base-content/70">
				That shouldn't happen — the built-in Soulseek and Usenet plugins load automatically. Check
				the server logs for <code class="font-mono text-xs">plugins.*</code> events.
			</p>
		</div>
	{:else}
		<ul class="space-y-3">
			{#each plugins as plugin (plugin.id)}
				{@const isOpen = openId === plugin.id}
				{@const result = testResults[plugin.id]}
				<li
					class="plugin-card overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50"
					class:is-active={plugin.loaded && plugin.enabled}
					class:is-broken={!plugin.loaded}
				>
					<div class="flex flex-wrap items-center gap-4 p-5">
						<div
							class="grid size-12 place-items-center rounded-2xl border border-base-content/8 bg-base-300/60"
						>
							{#if !plugin.loaded}
								<TriangleAlert class="size-6 text-error" aria-hidden="true" />
							{:else if plugin.builtin}
								<Puzzle class="size-6 text-accent" aria-hidden="true" />
							{:else if plugin.source === 'entry_point'}
								<Package class="size-6 text-accent" aria-hidden="true" />
							{:else}
								<FolderOpen class="size-6 text-accent" aria-hidden="true" />
							{/if}
						</div>
						<button
							type="button"
							class="min-w-0 flex-1 text-left"
							onclick={() => toggleOpen(plugin)}
							aria-expanded={isOpen}
							disabled={!plugin.loaded}
						>
							<div class="flex flex-wrap items-baseline gap-x-2.5 gap-y-1">
								<h3 class="font-display text-lg font-bold tracking-tight">{plugin.name}</h3>
								<span
									class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
									>{sourceLabel(plugin)}</span
								>
								{#if plugin.version}
									<span class="badge badge-ghost badge-sm font-mono tabular-nums"
										>v{plugin.version}</span
									>
								{/if}
							</div>
							<div class="flex items-center gap-2 text-sm text-base-content/70">
								{#if !plugin.loaded}
									<span class="truncate text-error" title={plugin.error ?? undefined}>
										Failed to load — {plugin.error}
									</span>
								{:else}
									<span
										class="orb"
										class:is-connected={plugin.enabled}
										role="status"
										aria-label={plugin.enabled ? 'Enabled' : 'Disabled'}
									></span>
									<span class="truncate">
										{plugin.enabled ? 'Enabled' : 'Disabled'} · source id
										<code class="font-mono text-xs">{plugin.id}</code>
									</span>
								{/if}
							</div>
						</button>
						{#if plugin.loaded}
							<label class="flex cursor-pointer items-center gap-2">
								<span class="text-sm font-medium">{plugin.enabled ? 'Enabled' : 'Disabled'}</span>
								<input
									type="checkbox"
									class="toggle toggle-accent"
									checked={plugin.enabled}
									disabled={toggle.isPending}
									onchange={() => setEnabled(plugin, !plugin.enabled)}
									aria-label={plugin.enabled ? `Disable ${plugin.name}` : `Enable ${plugin.name}`}
								/>
							</label>
							<button
								type="button"
								class="btn btn-ghost btn-sm btn-square"
								onclick={() => toggleOpen(plugin)}
								aria-label={isOpen ? 'Collapse' : 'Expand'}
							>
								<ChevronDown
									class={isOpen
										? 'size-5 rotate-180 transition-transform'
										: 'size-5 transition-transform'}
									aria-hidden="true"
								/>
							</button>
						{/if}
					</div>

					{#if isOpen && plugin.loaded}
						<div class="space-y-4 border-t border-base-content/8 p-5">
							{#if settingsQuery.isLoading}
								<div class="flex justify-center p-4">
									<span class="loading loading-spinner loading-sm text-base-content/40"></span>
								</div>
							{:else if schema.length === 0}
								<p class="text-sm text-base-content/50">This plugin has no settings.</p>
							{:else}
								<div class="space-y-3">
									{#each schema as field (field.key)}
										{@const id = fieldId(plugin.id, field.key)}
										{#if field.type === 'bool'}
											<label class="flex cursor-pointer items-center justify-between gap-4">
												<span class="min-w-0">
													<span class="block text-sm font-medium">
														{field.label || field.key}
														{#if field.required}<span class="text-error">*</span>{/if}
													</span>
													{#if field.help}
														<span class="block text-xs text-base-content/50">{field.help}</span>
													{/if}
												</span>
												<input
													type="checkbox"
													class="toggle toggle-accent"
													checked={Boolean(draft[field.key])}
													onchange={(e) =>
														(draft = { ...draft, [field.key]: e.currentTarget.checked })}
												/>
											</label>
										{:else}
											<div class="form-control">
												<label class="label" for={id}>
													<span class="label-text">
														{field.label || field.key}
														{#if field.required}<span class="text-error">*</span>{/if}
													</span>
												</label>
												{#if field.type === 'select'}
													<select
														{id}
														class="select select-bordered select-sm w-full"
														value={stringValue(field.key)}
														onchange={(e) => setSelect(field, e.currentTarget.value)}
													>
														{#each field.options as option (option.value)}
															<option value={String(option.value)}>
																{option.label || String(option.value)}
															</option>
														{/each}
													</select>
												{:else if field.type === 'int'}
													<input
														{id}
														type="number"
														class="input input-bordered input-sm w-full tabular-nums"
														value={stringValue(field.key)}
														oninput={(e) => setNumber(field.key, e.currentTarget.value)}
													/>
												{:else if field.type === 'secret'}
													<div class="join w-full">
														<input
															{id}
															type={revealed[field.key] ? 'text' : 'password'}
															class="input input-bordered input-sm join-item flex-1 font-mono text-sm"
															value={stringValue(field.key)}
															oninput={(e) =>
																(draft = { ...draft, [field.key]: e.currentTarget.value })}
														/>
														<button
															type="button"
															class="btn btn-sm join-item"
															onclick={() =>
																(revealed = { ...revealed, [field.key]: !revealed[field.key] })}
														>
															{revealed[field.key] ? 'Hide' : 'Show'}
														</button>
													</div>
												{:else}
													<input
														{id}
														type="text"
														class="input input-bordered input-sm w-full"
														value={stringValue(field.key)}
														oninput={(e) =>
															(draft = { ...draft, [field.key]: e.currentTarget.value })}
													/>
												{/if}
												{#if field.help}
													<span class="mt-1 text-xs text-base-content/50">{field.help}</span>
												{/if}
											</div>
										{/if}
									{/each}
								</div>

								<div class="flex flex-wrap items-center gap-3 pt-1">
									<button
										type="button"
										class="btn btn-sm"
										onclick={() => runTest(plugin)}
										disabled={test.isPending}
									>
										{test.isPending ? 'Testing…' : 'Test'}
									</button>
									{#if result}
										<span
											class="flex min-w-0 items-center gap-1.5 text-sm"
											class:text-success={result.valid}
											class:text-error={!result.valid}
										>
											{#if result.valid}
												<CircleCheck class="size-4 shrink-0" aria-hidden="true" />
											{:else}
												<CircleX class="size-4 shrink-0" aria-hidden="true" />
											{/if}
											<span class="truncate">{result.message}</span>
										</span>
									{/if}
									<div class="flex-1"></div>
									<button
										type="button"
										class="btn btn-primary btn-sm"
										onclick={() => saveDraft(plugin)}
										disabled={save.isPending}
									>
										{save.isPending ? 'Saving…' : 'Save'}
									</button>
								</div>
							{/if}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
		<p class="font-mono text-[0.68rem] uppercase tracking-[0.2em] text-base-content/35">
			plugin api v{apiVersion}
		</p>
	{/if}
</section>

<style>
	.plugin-card {
		transition:
			box-shadow 0.4s ease,
			border-color 0.4s ease;
	}
	.plugin-card.is-active {
		border-color: oklch(from var(--color-accent) l c h / 0.55);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.3),
			0 0 44px oklch(from var(--color-accent) l c h / 0.18);
	}
	.plugin-card.is-broken {
		border-color: oklch(from var(--color-error) l c h / 0.4);
	}
	.orb {
		display: inline-block;
		width: 0.7rem;
		height: 0.7rem;
		border-radius: 9999px;
		background: oklch(from var(--color-base-content) l c h / 0.3);
		transition: background 0.3s ease;
	}
	.orb.is-connected {
		background: var(--color-accent);
		animation: orb-pulse 2.4s ease-in-out infinite;
	}
	@keyframes orb-pulse {
		0%,
		100% {
			box-shadow: 0 0 5px oklch(from var(--color-accent) l c h / 0.5);
		}
		50% {
			box-shadow: 0 0 14px oklch(from var(--color-accent) l c h / 0.95);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.orb.is-connected {
			animation: none;
		}
	}
</style>
