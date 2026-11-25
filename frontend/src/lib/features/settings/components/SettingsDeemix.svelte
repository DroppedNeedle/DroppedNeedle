<script lang="ts">
	import { AudioLines, FolderTree, TriangleAlert } from 'lucide-svelte';

	import {
		getDeemixConfigQuery,
		getDeemixStatusQuery,
		createSaveDeemixConfigMutation,
		createTestDeemixMutation
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
	import type {
		DeemixConnectionSettings,
		DeemixQuality,
		DeemixStatus,
		DeemixWarning
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import { fillConnection, useSuggestion } from './setup/suggest.svelte';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const QUALITY: Record<DeemixQuality, string> = {
		flac: 'FLAC 16-bit',
		mp3_320: 'MP3 320',
		mp3_128: 'MP3 128'
	};

	const WARNING: Record<DeemixWarning, string> = {
		overwrite_off: 'Set Overwrite files to Yes in Deemix.',
		fallback_on: 'Deemix may lower quality. Turn off its fallback options.',
		single_user_off: "Keep Deemix's single-user mode on.",
		command_set: 'Deemix runs a command after downloads; it may move files DroppedNeedle needs.',
		mount_unusable: "Deemix's downloads folder is missing or not writable.",
		mount_overlaps: "Deemix's downloads folder overlaps another folder DroppedNeedle uses.",
		mapping_wrong: "Deemix's downloads aren't in the Downloads folder here. Check the mount.",
		quality_not_taken: "Your quality settings don't allow your Deezer account's quality."
	};

	const configQuery = getDeemixConfigQuery();
	const statusQuery = getDeemixStatusQuery();
	const test = createTestDeemixMutation();

	let testResult = $state<DeemixStatus | null>(null);

	// An address the setup guide found, filled in while none is set.
	let { suggested }: { suggested?: string } = $props();

	const section = createSettingsSection(configQuery, createSaveDeemixConfigMutation(), {
		normalize: (draft) => ({
			...draft,
			downloads_mount: draft.downloads_mount || '/deemix-downloads'
		}),
		test: async (draft) => {
			testResult = null;
			if (!draft.url.trim()) return { passed: false, message: 'Add the URL first.' };
			const result = await test.mutateAsync(draft);
			testResult = result;
			if (!result.reachable) return { passed: false, message: result.message };
			// Another address is read on a new session, which shows no account until saved.
			const unsaved = draft.url !== configQuery.data?.url;
			const ready = result.signed_in || result.sign_in_offered || unsaved;
			const problem = warningText(result);
			return {
				passed: result.deezer_reachable && ready && problem === null,
				message: problem ?? summary(result)
			};
		}
	});
	useSuggestion(
		section,
		() => suggested,
		(draft, url) => fillConnection(draft, url, true)
	);

	function summary(live: DeemixStatus): string {
		if (!live.reachable || !live.deezer_reachable || !live.signed_in) return live.message;
		const version = live.webui_version ? ` · v${live.webui_version}` : '';
		const account = live.account_quality
			? ` · Deezer account: ${QUALITY[live.account_quality]}`
			: '';
		return `Connected${version}${account}`;
	}

	// The first thing to change: the server's warnings come in a fixed order.
	function warningText(live: DeemixStatus | null | undefined): string | null {
		const first = live?.warnings[0];
		if (!first) return null;
		if (first === 'quality_not_taken' && live?.account_quality) {
			return `Your Deezer account gives ${QUALITY[live.account_quality]}; your quality settings don't allow it.`;
		}
		return WARNING[first];
	}

	// A Test result shows until the status reads again, as it does after a save.
	$effect(() => {
		void statusQuery.dataUpdatedAt;
		testResult = null;
	});

	const live = $derived(testResult ?? statusQuery.data);
	const warning = $derived(warningText(live));

	function statusText(enabled: boolean, url: string): string {
		if (live && (live.message || live.reachable)) return summary(live);
		if (!enabled) return 'Disabled';
		return url ? 'Run Test to check the connection' : 'Not configured';
	}

	// The enable switch sits in the collapsed header, so it saves at once rather
	// than waiting for Save. It is switched back if the save fails.
	async function onToggle(draft: DeemixConnectionSettings) {
		await section.save();
		if (section.status?.tone === 'success') {
			toastStore.show({
				message: `Deemix ${draft.enabled ? 'enabled' : 'disabled'}`,
				type: 'success'
			});
		} else {
			draft.enabled = !draft.enabled;
			toastStore.show({ message: 'Could not update Deemix', type: 'error' });
		}
	}
</script>

<QueryBoundary query={section} what="the Deemix settings">
	{#snippet loading()}
		<div class="skeleton h-28 w-full rounded-box"></div>
	{/snippet}
	{#snippet children(draft)}
		<DownloadClientCard
			open={suggested !== undefined}
			title="Deemix"
			sourceLabel="Deezer"
			icon={AudioLines}
			connected={live?.reachable === true && live.signed_in}
			statusText={statusText(draft.enabled, draft.url)}
			bind:enabled={draft.enabled}
			onToggle={() => void onToggle(draft)}
			enableAriaLabel="Enable Deemix download client"
		>
			{#if warning}
				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<p>{warning}</p>
				</div>
			{/if}

			<TextField
				label="Deemix URL"
				mono
				bind:value={draft.url}
				placeholder="http://deemix:6595"
				error={section.fieldError('url', 'Deemix URL')}
			/>

			<p class="text-xs leading-relaxed text-muted">
				DroppedNeedle uses the Deezer sign-in saved in Deemix. Sign in on Deemix's own page.
			</p>

			<section class="space-y-2">
				<div class="flex items-center gap-2 text-sm font-semibold">
					<FolderTree class="size-4 text-muted" aria-hidden="true" /> Downloads folder
				</div>
				<Card variant="bordered" padding="sm">
					<div class="space-y-1.5">
						<label class="text-sm font-medium" for="deemix-mount">Downloads folder</label>
						<p class="text-xs text-muted">
							Where DroppedNeedle sees Deemix's downloads folder, mounted read-write into this
							container on the same disk as your library.
						</p>
						<Input
							id="deemix-mount"
							label="Downloads folder"
							size="sm"
							mono
							class="w-full"
							bind:value={draft.downloads_mount}
							placeholder="/deemix-downloads"
						/>
					</div>
				</Card>
			</section>

			<SaveBar {...section.bar} testLabel="Test connection" />
		</DownloadClientCard>
	{/snippet}
</QueryBoundary>
