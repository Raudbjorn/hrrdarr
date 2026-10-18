<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, MediaDomain, Tag, DownloadScope, Provider, ProviderImplementation, ProviderInput, ProviderSettings, ProviderTemplate, ProviderTestResult, NzbgetPriority, NzbgetScope, TorrentLabelScope, TorrentPlacement, TorrentQueuePriority, TransmissionScope, UsenetPriority, UsenetScope } from './api.generated';
  import {eligibleDownloadClient,indexerOptions,isClientImplementation,matchesPendingProvider,parseTransmissionEndpoint,transmissionEndpoint,transmissionPlacementConflict,transmissionPlacementProblem,transmissionRpcUrl,TRANSMISSION_DEFAULT_PORT,TRANSMISSION_DEFAULT_URL_BASE,type PendingProvider} from './provider-draft';
  import type {SettingsWriteState} from './api';
  import {listTags, readDownloadHandling, listProviders, getProvider, getProviderSchema, createProvider, updateProvider, deleteProvider, testProvider } from './api';
  let {relatedWrite='idle',tagVersions={tv:0,movies:0},onwrite=()=>{},onchange=()=>{}}:{relatedWrite?:SettingsWriteState;tagVersions?:Record<MediaDomain,number>;onwrite?:(state:SettingsWriteState)=>void;onchange?:()=>void}=$props();
  $effect(()=>onwrite(busy?'busy':uncertain?'uncertain':'idle'));
  function discard(){return !busy&&relatedWrite==='idle'&&(!dirty||window.confirm('Discard unsaved provider changes?'));}
  let pendingCreation=$state<PendingProvider|null>(null);
  let page: ApiPage<Provider> | null = $state(null), templates: ProviderTemplate[] = $state([]), selected: Provider | null = $state(null);
  let editing = $state(false), implementation = $state<ProviderImplementation>('torznab'), name = $state(''), endpoint = $state(''), enabled = $state(true), priority = $state(1);
  let tv = $state(true), movies = $state(false), tvCategories = $state(''), animeCategories = $state(''), movieCategories = $state(''), tvCategory = $state(''), movieCategory = $state('');
  let tvOptions=$state(indexerOptions(null)), movieOptions=$state(indexerOptions(null));
  const usenetPriorities:{value:UsenetPriority;label:string}[]=[{value:'default',label:'Client default'},{value:'paused',label:'Paused'},{value:'low',label:'Low'},{value:'normal',label:'Normal'},{value:'high',label:'High'},{value:'force',label:'Force'}];
  let usenetPriority=$state<Record<MediaDomain,{recent:UsenetPriority;older:UsenetPriority}>>({tv:{recent:'default',older:'default'},movies:{recent:'default',older:'default'}});
  // NZBGet: a category and NZBGet's own job priority scale per domain, plus add-paused.
  const nzbgetPriorities:{value:NzbgetPriority;label:string}[]=[{value:'very_low',label:'Very low'},{value:'low',label:'Low'},{value:'normal',label:'Normal'},{value:'high',label:'High'},{value:'very_high',label:'Very high'},{value:'force',label:'Force'}];
  let nzbgetPriority=$state<Record<MediaDomain,{recent:NzbgetPriority;older:NzbgetPriority}>>({tv:{recent:'normal',older:'normal'},movies:{recent:'normal',older:'normal'}});
  const isNzbget=$derived(implementation==='nzbget');
  // Deluge (label-isolated torrent client): a lowercase label, queue placement and add-paused per domain.
  const isLabel=$derived(implementation==='deluge'||implementation==='rtorrent');
  const isClient=$derived(isClientImplementation(implementation));
  // Transmission: per-domain placement (category subfolder/label or absolute directory), queue
  // priority and add-paused; the endpoint is composed from host, port, SSL and URL base.
  const queuePriorities:{value:TorrentQueuePriority;label:string}[]=[{value:'last',label:'Last (queue order)'},{value:'first',label:'First (top of queue)'}];
  let placement=$state<Record<MediaDomain,TorrentPlacement['strategy']>>({tv:'category',movies:'category'}),tvDirectory=$state(''),movieDirectory=$state('');
  let torrentPriority=$state<Record<MediaDomain,{recent:TorrentQueuePriority;older:TorrentQueuePriority}>>({tv:{recent:'last',older:'last'},movies:{recent:'last',older:'last'}});
  let addPaused=$state<Record<MediaDomain,boolean>>({tv:false,movies:false});
  let txHost=$state(''),txPort=$state(TRANSMISSION_DEFAULT_PORT),txSsl=$state(false),txUrlBase=$state(TRANSMISSION_DEFAULT_URL_BASE);
  const isTransmission=$derived(implementation==='transmission');
  const txEndpoint=$derived(transmissionEndpoint({host:txHost,port:Number(txPort),useSsl:txSsl,urlBase:txUrlBase}));
  function fillTransmission(scope:TransmissionScope|null|undefined,domain:MediaDomain){
    placement[domain]=scope?.placement.strategy??'category';
    const category=scope?.placement.strategy==='category'?scope.placement.category:'',directory=scope?.placement.strategy==='directory'?scope.placement.directory:'';
    if(domain==='tv'){tvCategory=category;tvDirectory=directory;}else{movieCategory=category;movieDirectory=directory;}
    torrentPriority[domain]={recent:scope?.recent_priority??'last',older:scope?.older_priority??'last'};addPaused[domain]=scope?.add_paused??false;
  }
  let clientTags=$state<Record<MediaDomain,Tag[]>>({tv:[],movies:[]}),tvTags=$state<number[]>([]),movieTags=$state<number[]>([]);
  let tagLoading=$state(false),tagLoaded=$state(false),tagError=$state(''),loadedTagVersions=$state({tv:-1,movies:-1}),tagEpoch=0;
  const tagsStale=$derived(loadedTagVersions.tv!==tagVersions.tv||loadedTagVersions.movies!==tagVersions.movies);
  async function loadClientTags(){
    if(locked)return;const token=++tagEpoch,versions={...tagVersions};tagLoading=true;tagError='';
    const results=await Promise.all([listTags('tv'),listTags('movies')]);if(!alive||token!==tagEpoch)return;tagLoading=false;
    if(results[0].ok&&results[1].ok){clientTags={tv:results[0].data,movies:results[1].data};loadedTagVersions=versions;tagLoaded=true;}
    else{const failed=results.find(r=>!r.ok);tagError=failed&&!failed.ok?failed.error:'Tag catalog could not be read';}
  }
  let credentialAction = $state('preserve'), credentialKind = $state<'api_key' | 'username_password'>('api_key'), apiKey = $state(''), username = $state(''), password = $state('');
  let loading = $state(false), busy = $state(false), dirty = $state(false), confirmingDelete = $state(false), conflict = $state(false), uncertain = $state(false);
  let error = $state(''), notice = $state(''), testResult: ProviderTestResult | null = $state(null);
  const locked=$derived(busy||uncertain||relatedWrite!=='idle');
  let alive = true, selectionVersion = 0, listVersion = 0;
  const template = $derived(templates.find(item => item.implementation === implementation));
  function clearSecrets() { apiKey = ''; username = ''; password = ''; }
  function fill(item: Provider | null) {
    pendingCreation=null;selected = item; editing = true; confirmingDelete = false; conflict = false; uncertain = false; error = ''; notice = ''; testResult = null; clearSecrets(); credentialAction = 'preserve';
    if (item) {
      implementation = item.settings.implementation; name = item.name; endpoint = item.settings.endpoint; enabled = item.enabled; priority = item.priority; tv = !!item.settings.tv; movies = !!item.settings.movies;
      if (item.settings.implementation === 'transmission') {const address=parseTransmissionEndpoint(item.settings.endpoint);txHost=address?.host??'';txPort=address?.port??TRANSMISSION_DEFAULT_PORT;txSsl=address?.useSsl??false;txUrlBase=address?.urlBase??'';tvTags=[...(item.settings.tv?.tags??[])];movieTags=[...(item.settings.movies?.tags??[])];fillTransmission(item.settings.tv,'tv');fillTransmission(item.settings.movies,'movies');tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else if (item.settings.implementation === 'deluge' || item.settings.implementation === 'rtorrent') {const tvScope=item.settings.tv, movieScope=item.settings.movies;tvTags=[...(tvScope?.tags??[])];movieTags=[...(movieScope?.tags??[])];tvCategory = tvScope?.category ?? ''; movieCategory = movieScope?.category ?? ''; torrentPriority={tv:{recent:tvScope?.recent_priority??'last',older:tvScope?.older_priority??'last'},movies:{recent:movieScope?.recent_priority??'last',older:movieScope?.older_priority??'last'}}; addPaused={tv:tvScope?.add_paused??false,movies:movieScope?.add_paused??false}; tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else if (item.settings.implementation === 'nzbget') {const tvScope=item.settings.tv, movieScope=item.settings.movies;tvTags=[...(tvScope?.tags??[])];movieTags=[...(movieScope?.tags??[])];tvCategory = tvScope?.category ?? ''; movieCategory = movieScope?.category ?? ''; nzbgetPriority={tv:{recent:tvScope?.recent_priority??'normal',older:tvScope?.older_priority??'normal'},movies:{recent:movieScope?.recent_priority??'normal',older:movieScope?.older_priority??'normal'}}; addPaused={tv:tvScope?.add_paused??false,movies:movieScope?.add_paused??false}; tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else if (item.settings.implementation === 'sabnzbd') {const tvScope=item.settings.tv, movieScope=item.settings.movies;tvTags=[...(tvScope?.tags??[])];movieTags=[...(movieScope?.tags??[])];tvCategory = tvScope?.category ?? ''; movieCategory = movieScope?.category ?? ''; usenetPriority={tv:{recent:tvScope?.recent_priority??'default',older:tvScope?.older_priority??'default'},movies:{recent:movieScope?.recent_priority??'default',older:movieScope?.older_priority??'default'}}; tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else if (item.settings.implementation === 'qbittorrent') {tvTags=[...(item.settings.tv?.tags??[])];movieTags=[...(item.settings.movies?.tags??[])];tvCategory = item.settings.tv?.category ?? ''; movieCategory = item.settings.movies?.category ?? ''; tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else {tvOptions=indexerOptions(item.settings.tv);movieOptions=indexerOptions(item.settings.movies);tvCategories = item.settings.tv?.categories.join(', ') ?? ''; animeCategories = item.settings.tv?.anime_categories.join(', ') ?? ''; movieCategories = item.settings.movies?.categories.join(', ') ?? ''; tvCategory = ''; movieCategory = '';}
    } else {name = ''; endpoint = ''; tv = true; movies = false; tvCategory = ''; movieCategory = ''; tvDirectory=''; movieDirectory=''; defaults();}
    credentialKind = implementation === 'qbittorrent' || implementation === 'transmission' || implementation === 'nzbget' || implementation === 'rtorrent' ? 'username_password' : 'api_key';
    dirty = !item;
  }
  function defaults() {
    const current = templates.find(item => item.implementation === implementation);
    tvTags=[];movieTags=[];
    const usenet=current?.defaults.kind==='usenet_client'?current.defaults:null;
    usenetPriority={tv:{recent:usenet?.recent_priority??'default',older:usenet?.older_priority??'default'},movies:{recent:usenet?.recent_priority??'default',older:usenet?.older_priority??'default'}};
    const nzb=current?.defaults.kind==='nzbget_client'?current.defaults:null;
    nzbgetPriority={tv:{recent:nzb?.recent_priority??'normal',older:nzb?.older_priority??'normal'},movies:{recent:nzb?.recent_priority??'normal',older:nzb?.older_priority??'normal'}};
    const torrent=current?.defaults.kind==='transmission_client'||current?.defaults.kind==='label_torrent_client'?current.defaults:null;
    torrentPriority={tv:{recent:torrent?.recent_priority??'last',older:torrent?.older_priority??'last'},movies:{recent:torrent?.recent_priority??'last',older:torrent?.older_priority??'last'}};
    addPaused={tv:torrent?.add_paused??nzb?.add_paused??false,movies:torrent?.add_paused??nzb?.add_paused??false};placement={tv:'category',movies:'category'};
    txHost='';txPort=TRANSMISSION_DEFAULT_PORT;txSsl=false;txUrlBase=TRANSMISSION_DEFAULT_URL_BASE;
    tvOptions=indexerOptions(current?.defaults.kind==='indexer'?current.defaults.tv:null);
    movieOptions=indexerOptions(current?.defaults.kind==='indexer'?current.defaults.movies:null);
    enabled = current?.enabled ?? true; priority = current?.priority ?? 1;
    tvCategories = current?.defaults.kind === 'indexer' ? current.defaults.tv.categories.join(', ') : '';
    animeCategories = current?.defaults.kind === 'indexer' ? current.defaults.tv.anime_categories.join(', ') : '';
    movieCategories = current?.defaults.kind === 'indexer' ? current.defaults.movies.categories.join(', ') : '';
  }
  function changeImplementation() {clearSecrets(); credentialKind = implementation === 'qbittorrent' || implementation === 'transmission' || implementation === 'nzbget' || implementation === 'rtorrent' ? 'username_password' : 'api_key'; credentialAction = 'preserve'; defaults(); dirty = true;}
  async function load(offset = 0) {
    const version = ++listVersion; loading = true;
    const result = await listProviders(offset);
    if (!alive || version !== listVersion) return;
    loading = false; if (result.ok) page = result.data; else error = result.error;
  }
  async function select(id: string) {
    if(uncertain&&selected&&id!==selected.id){error='Reload the provider with the unresolved write before selecting another provider.';return;}
    if (!discard()) return;
    const version=++selectionVersion;busy=true;error='';
    const result=await readDownloadHandling(id);
    if(!alive||version!==selectionVersion)return;
    busy=false;
    if(result.ok){if(uncertain&&!selected&&pendingCreation){if(!result.data.provider||!matchesPendingProvider(result.data.provider,pendingCreation)){error='This saved provider does not match the pending creation. Review matching records; another creation will not be sent.';return;}if(!window.confirm(`Adopt saved provider ${id} to resolve the unknown creation? Matching configuration does not prove this request created it. Review any duplicate records separately.`))return;}if(result.data.provider)fill(result.data.provider);else{selected=null;editing=false;dirty=false;uncertain=false;conflict=false;clearSecrets();notice='Provider is no longer present; related settings checked.';}onchange();}
    else error=result.error;
  }
  function categories(value: string): number[] | null {
    if (!value.trim()) return [];
    const parts = value.split(',').map(part => part.trim());
    if (parts.length > 64 || parts.some(part => !/^[1-9]\d*$/.test(part))) return null;
    const numbers = parts.map(Number);
    return numbers.some(id => !Number.isSafeInteger(id) || id > 2147483647) || new Set(numbers).size !== numbers.length ? null : numbers;
  }
  function downloadScope(category: string, tags:number[], existing: DownloadScope | null | undefined): DownloadScope | null {
    if (existing) return {...existing, category, tags};
    if (template?.defaults.kind !== 'download_client') return null;
    const {kind: _, ...defaults} = template.defaults;
    return {...defaults, category, tags};
  }
  function usenetScope(domain: MediaDomain, category: string, tags:number[], existing: UsenetScope | null | undefined): UsenetScope | null {
    const {recent, older} = usenetPriority[domain];
    if (existing) return {...existing, category, tags, recent_priority:recent, older_priority:older};
    if (template?.defaults.kind !== 'usenet_client') return null;
    const {kind: _, ...defaults} = template.defaults;
    return {...defaults, category, tags, recent_priority:recent, older_priority:older};
  }
  function input(): ProviderInput | null {
    if (!tv && !movies) {error = 'Enable at least one media scope.'; return null;}
    let settings: ProviderSettings;
    if (implementation === 'transmission') {
      if(!tagLoaded||tagLoading||tagsStale||tagError){error='Reload the client tag choices before saving.';return null;}
      if(!txEndpoint){error='Enter a host name or address, a port between 1 and 65535 and a URL base such as /transmission.';return null;}
      const placed=(domain:MediaDomain):TorrentPlacement=>placement[domain]==='category'?{strategy:'category',category:(domain==='tv'?tvCategory:movieCategory).trim()}:{strategy:'directory',directory:(domain==='tv'?tvDirectory:movieDirectory).trim()};
      for(const domain of [tv?'tv':null,movies?'movies':null].filter((d):d is MediaDomain=>d!==null)){const problem=transmissionPlacementProblem(placed(domain));if(problem){error=`${domain==='tv'?'TV':'Movie'} placement: ${problem}`;return null;}}
      const conflict=tv&&movies?transmissionPlacementConflict(placed('tv'),placed('movies')):null;if(conflict){error=conflict;return null;}
      const saved = selected?.settings.implementation === 'transmission' ? selected.settings : null;
      const scope=(domain:MediaDomain,tags:number[]):TransmissionScope=>({...(saved?.[domain]??{}),placement:placed(domain),recent_priority:torrentPriority[domain].recent,older_priority:torrentPriority[domain].older,add_paused:addPaused[domain],tags});
      endpoint=txEndpoint;
      settings = {implementation, endpoint:txEndpoint, tv:tv?scope('tv',tvTags):null, movies:movies?scope('movies',movieTags):null};
    } else if (implementation === 'deluge' || implementation === 'rtorrent') {
      if(!tagLoaded||tagLoading||tagsStale||tagError){error='Reload the client tag choices before saving.';return null;}
      for(const [on,label,who] of [[tv,tvCategory,'TV'],[movies,movieCategory,'Movie']] as const){if(on&&!/^[a-z0-9._-]{1,64}$/.test(label.trim())){error=`${who} label: use 1 to 64 lowercase letters, digits, dots, underscores or hyphens (Deluge's Label plugin accepts nothing else, and the other label clients share the rule).`;return null;}}
      if (tv && movies && tvCategory.trim() === movieCategory.trim()) {error = 'TV and movie labels must differ so each domain only sees its own downloads.'; return null;}
      const saved = selected?.settings.implementation === 'deluge' || selected?.settings.implementation === 'rtorrent' ? selected.settings : null;
      const scope=(domain:MediaDomain,category:string,tags:number[]):TorrentLabelScope=>({...(saved?.[domain]??{}),category:category.trim(),tags,recent_priority:torrentPriority[domain].recent,older_priority:torrentPriority[domain].older,add_paused:addPaused[domain]});
      settings = {implementation, endpoint, tv:tv?scope('tv',tvCategory,tvTags):null, movies:movies?scope('movies',movieCategory,movieTags):null};
    } else if (implementation === 'nzbget') {
      if(!tagLoaded||tagLoading||tagsStale||tagError){error='Reload the client tag choices before saving.';return null;}
      if (tv && movies && tvCategory.trim().toLowerCase() === movieCategory.trim().toLowerCase()) {error = 'TV and movie categories must differ so each domain only sees its own downloads.'; return null;}
      const saved = selected?.settings.implementation === 'nzbget' ? selected.settings : null;
      const scope=(domain:MediaDomain,category:string,tags:number[]):NzbgetScope=>({...(saved?.[domain]??{}),category:category.trim(),tags,recent_priority:nzbgetPriority[domain].recent,older_priority:nzbgetPriority[domain].older,add_paused:addPaused[domain]});
      settings = {implementation, endpoint, tv:tv?scope('tv',tvCategory,tvTags):null, movies:movies?scope('movies',movieCategory,movieTags):null};
    } else if (implementation === 'sabnzbd') {
      if(!tagLoaded||tagLoading||tagsStale||tagError){error='Reload the client tag choices before saving.';return null;}
      if (tv && movies && tvCategory.trim().toLowerCase() === movieCategory.trim().toLowerCase()) {error = 'TV and movie categories must differ so each domain only sees its own downloads.'; return null;}
      const saved = selected?.settings.implementation === 'sabnzbd' ? selected.settings : null;
      const tvScope = tv ? usenetScope('tv', tvCategory, tvTags, saved?.tv) : null, movieScope = movies ? usenetScope('movies', movieCategory, movieTags, saved?.movies) : null;
      if ((tv && !tvScope) || (movies && !movieScope)) {error = 'Provider defaults are unavailable. Reload provider settings.'; return null;}
      settings = {implementation, endpoint, tv:tvScope, movies:movieScope};
    } else if (implementation === 'qbittorrent') {
      if(!tagLoaded||tagLoading||tagsStale||tagError){error='Reload the client tag choices before saving.';return null;}
      const saved = selected?.settings.implementation === 'qbittorrent' ? selected.settings : null;
      const tvScope = tv ? downloadScope(tvCategory, tvTags, saved?.tv) : null, movieScope = movies ? downloadScope(movieCategory, movieTags, saved?.movies) : null;
      if ((tv && !tvScope) || (movies && !movieScope)) {error = 'Provider defaults are unavailable. Reload provider settings.'; return null;}
      settings = {implementation, endpoint, tv:tvScope, movies:movieScope};
    } else {
      const tvIds = categories(tvCategories), animeIds = categories(animeCategories), movieIds = categories(movieCategories);
      if ((tv && (!tvIds || !animeIds)) || (movies && !movieIds)) {error = 'Categories must be distinct positive numeric IDs separated by commas (maximum 64 per field).'; return null;}
      const saved = selected && (selected.settings.implementation === 'torznab' || selected.settings.implementation === 'newznab') ? selected.settings : null;
      const defaults = template?.defaults.kind === 'indexer' ? template.defaults : null;
      if ((!saved?.tv && tv || !saved?.movies && movies) && !defaults) {error = 'Provider defaults are unavailable. Reload provider settings.'; return null;}
      settings = {implementation, endpoint, tv:tv ? {...(saved?.tv ?? defaults!.tv), ...tvOptions, categories:tvIds!, anime_categories:animeIds!} : null, movies:movies ? {...(saved?.movies ?? defaults!.movies), ...movieOptions, categories:movieIds!} : null};
    }
    if (selected?.has_credentials && endpoint !== selected.settings.endpoint && credentialAction === 'preserve') {error = 'The endpoint changed. Explicitly replace or clear credentials before saving to another destination.'; return null;}
    return {name, enabled, priority, settings, ...(credentialAction === 'clear' ? {credentials:null} : credentialAction === 'replace' ? {credentials:(implementation === 'qbittorrent' && credentialKind === 'username_password') || implementation === 'transmission' || implementation === 'nzbget' || implementation === 'rtorrent' ? {kind:'username_password' as const,username,password} : {kind:'api_key' as const,api_key:apiKey}} : {})};
  }
  async function save() {
    if (locked || conflict) return;
    error = ''; notice = ''; const body = input(); if (!body) return;
    const version = selectionVersion; busy = true;
    if(!selected){const {credentials:_,...publicConfig}=body;pendingCreation=publicConfig;}
    const result = selected ? await updateProvider(selected.id, {...body, revision:selected.revision}) : await createProvider(body);
    clearSecrets();
    if (!alive || version !== selectionVersion) return;
    busy = false;
    if (result.ok) {fill(result.data);notice='Provider saved. Test it to check the saved connection.';busy=true;const checked=await readDownloadHandling(result.data.id);busy=false;if(!checked.ok){uncertain=true;error='Provider saved, but related settings readback failed. Reload saved provider.';}onchange();void load(page?.offset??0);}
    else {error = result.error; conflict = result.status === 409; uncertain = result.status===undefined||result.status>=500; if (uncertain) error += ' The response was lost or unreadable. Refresh the list and reload the saved provider before another save; the change may have committed.';}
  }
  async function test() {
    if (!selected || locked || dirty || conflict) return;
    const id = selected.id, revision = selected.revision, version = selectionVersion; busy = true; error = ''; notice = ''; testResult = null;
    const result = await testProvider(id);
    if (!alive || version !== selectionVersion) return;
    // Read back even failed/uncertain tests: a response timeout does not erase an observation.
    const current = await getProvider(id);
    if (!alive || version !== selectionVersion) return;
    busy = false;
    if (current.ok) {
      selected = current.data;
      if (current.data.revision !== revision) {conflict = true; error = 'Provider changed during testing. Reload the saved provider before editing or testing again.'; return;}
    }
    if (result.ok && result.data.provider_id === id && result.data.revision === revision) {testResult = result.data; notice = 'Saved connection test completed.';}
    else error = result.ok ? 'Test result did not match the selected provider revision.' : `${/[.!?]$/.test(result.error.trim()) ? result.error.trim() : `${result.error.trim()}.`} The saved observation below may describe an earlier test; no successful result is assumed.`;
    if (!current.ok) error += `${error ? ' ' : ''}Could not refresh the saved observation. Reload the saved provider to check it.`;
    void load(page?.offset ?? 0);
  }
  async function remove() {
    if (!selected || locked || !confirmingDelete || conflict) return;
    const id = selected.id, revision = selected.revision, version = selectionVersion; busy = true; error = ''; clearSecrets();
    const result = await deleteProvider(id, revision);
    if (!alive || version !== selectionVersion) return;
    busy = false; confirmingDelete = false;
    if(result.ok){busy=true;const checked=await readDownloadHandling(id);busy=false;if(checked.ok){selected=null;editing=false;dirty=false;notice='Provider configuration deleted. Media and remote downloads were not deleted.';}else{uncertain=true;error='Delete accepted; related settings readback failed. Reload saved provider.';}onchange();void load();}
    else {error = result.error; conflict = result.status === 409; uncertain = result.status===undefined||result.status>=500; if (uncertain) error += ' Deletion may have completed. Refresh the list before retrying.';}
  }
  onMount(() => {
    alive = true; void load(); void loadClientTags();
    void Promise.all([getProviderSchema('tv','indexer'),getProviderSchema('tv','download_client')]).then(results => {
      if (!alive) return;
      templates = results.flatMap(result => result.ok ? result.data.templates : []);
      const failed = results.find(result => !result.ok); if (failed && !failed.ok) error = failed.error;
    });
    return () => {alive = false; ++selectionVersion; ++listVersion; ++tagEpoch; clearSecrets();};
  });
</script>
<svelte:window onbeforeunload={event=>{if(dirty||busy||uncertain){event.preventDefault();event.returnValue='';}}} />

<section aria-labelledby="providers-heading">
  {#if uncertain&&pendingCreation}<p role="alert">Creation outcome unknown. Browse saved providers and explicitly adopt a matching configuration. Matching records are shown separately by ID; none is assumed to be this request’s result. A new creation will not be sent while unresolved.</p>{/if}
  {#if relatedWrite!=='idle'}<p role="alert">Related download settings or tags have a pending or unknown write. Resolve it before changing providers.</p>{/if}
  <div class="heading"><div><h1 id="providers-heading">Providers</h1><p>Configure indexers and download clients for TV, movies or both.</p></div><button class="primary" disabled={locked || !templates.length} onclick={() => {if(!discard())return;++selectionVersion; implementation = 'torznab'; fill(null);}}>New provider</button></div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  <div class="providers-workspace">
    <aside aria-label="Saved providers"><button disabled={loading} onclick={() => load(page?.offset ?? 0)}>Refresh providers</button>{#if loading}<p role="status">Loading providers…</p>{/if}
      {#if page}<p>{page.total} configured</p><ul>{#each page.items as provider (provider.id)}<li><button disabled={busy || relatedWrite!=='idle'} aria-current={selected?.id === provider.id ? 'true' : undefined} onclick={() => select(provider.id)}><strong>{provider.name}</strong>{#if uncertain&&pendingCreation}<small>ID {provider.id}{matchesPendingProvider(provider,pendingCreation)?' — matches pending configuration':''}</small>{/if}<span>{provider.settings.implementation} / {provider.settings.tv ? 'TV' : ''}{provider.settings.tv && provider.settings.movies ? ' + ' : ''}{provider.settings.movies ? 'Movies' : ''}</span><span>{provider.enabled ? 'Enabled' : 'Disabled'} · {provider.test_status.replaceAll('_',' ')}</span></button></li>{:else}<li>No providers yet. Add an indexer or download client to begin.</li>{/each}</ul><div class="pagination"><button disabled={loading || page.offset === 0} onclick={() => load(Math.max(0,page!.offset - 25))}>Previous providers</button><button disabled={loading || page.offset + page.limit >= page.total} onclick={() => load(page!.offset + page!.limit)}>Next providers</button></div>{/if}
    </aside>
    <article aria-label="Provider editor">
      {#if busy}<p role="status">Working on provider…</p>{/if}
      {#if editing}
        <h2>{selected ? selected.name : 'New provider'}</h2>
        {#if selected}<p>Saved revision {selected.revision}. Credentials: {selected.has_credentials ? 'stored (write-only)' : 'none stored'}.</p><p>Saved test: {selected.test_status.replaceAll('_',' ')}{selected.last_test ? ` at ${new Date(selected.last_test.tested_at * 1000).toLocaleString()} (revision ${selected.last_test.revision})` : ''}{selected.last_test?.error_code ? `: ${selected.last_test.error_code}` : ''}.</p><button disabled={busy || relatedWrite!=='idle'} onclick={() => select(selected!.id)}>Reload saved provider</button>{/if}
        <!-- Clear stale panels while editing, before blur can move Save between pointerdown and click. -->
        <form onsubmit={(event) => {event.preventDefault(); void save();}} oninput={() => {dirty = true; confirmingDelete = false; testResult = null;}} onchange={() => {dirty = true; confirmingDelete = false; testResult = null;}}>
          <fieldset disabled={locked || conflict}><legend>Connection</legend>
            <label>Provider type<select bind:value={implementation} disabled={!!selected} onchange={changeImplementation}><option value="torznab">Torznab</option><option value="newznab">Newznab</option><option value="qbittorrent">qBittorrent</option><option value="sabnzbd">SABnzbd</option><option value="nzbget">NZBGet</option><option value="transmission">Transmission</option><option value="deluge">Deluge</option><option value="rtorrent">rTorrent</option></select></label>
            <label>Name<input bind:value={name} required maxlength="128" /></label>{#if isTransmission}<label>Host<input bind:value={txHost} required maxlength="253" autocomplete="off" placeholder="transmission.example" /></label><label>Port<input type="number" bind:value={txPort} required min="1" max="65535" step="1" /></label><label class="check"><input type="checkbox" bind:checked={txSsl} />Use SSL</label><label>URL base<input bind:value={txUrlBase} maxlength="512" placeholder="/transmission" /></label><p>{txEndpoint ? `RPC URL: ${transmissionRpcUrl(txEndpoint)}` : 'Enter a host, port and URL base to form the RPC URL.'}</p>{:else}<label>Endpoint<input type="url" bind:value={endpoint} required maxlength="2048" placeholder={implementation === 'sabnzbd' ? 'http://sabnzbd.example:8080/sabnzbd' : implementation === 'nzbget' ? 'http://nzbget.example:6789' : implementation === 'deluge' ? 'http://deluge.example:8112' : implementation === 'rtorrent' ? 'http://rtorrent.example/RPC2' : implementation === 'qbittorrent' ? 'http://qbittorrent.example:8080' : 'https://indexer.example/api'} /></label>{/if}
            {#if implementation === 'sabnzbd'}<p>Include any URL base (for example /sabnzbd); the API path is added automatically.</p>{/if}{#if isNzbget}<p>Include any URL base; the JSON-RPC path /jsonrpc is added automatically.</p>{/if}{#if implementation === 'deluge'}<p>Use the Deluge Web UI address; the JSON-RPC path /json is added automatically. The Label plugin must be enabled.</p>{/if}{#if implementation === 'rtorrent'}<p>Use the full XML-RPC URL of the web server in front of rTorrent (for example /RPC2); nothing is appended.</p>{/if}<p>Use an endpoint without credentials. Saving does not contact it; Test saved provider contacts every configured scope.</p>
            <label class="check"><input type="checkbox" bind:checked={enabled} />Enabled</label><label>Priority<input type="number" bind:value={priority} min="1" max="100" step="1" required /></label>
          </fieldset>
          <fieldset disabled={locked || conflict}><legend>Media scopes</legend>
            <label class="check"><input type="checkbox" bind:checked={tv} />TV scope</label>
            {#if tv}{#if isClient}{#if isTransmission}<label>TV placement<select bind:value={placement.tv}><option value="category">Category subfolder and label</option><option value="directory">Download directory</option></select></label>{#if placement.tv==='category'}<label>TV download category<input bind:value={tvCategory} required maxlength="64" /></label>{:else}<label>TV download directory<input bind:value={tvDirectory} required maxlength="1024" placeholder="/downloads/tv" /></label>{/if}<label>TV recent priority<select bind:value={torrentPriority.tv.recent}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>TV older priority<select bind:value={torrentPriority.tv.older}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.tv} />TV add paused</label>{:else}<label>TV download category<input bind:value={tvCategory} required maxlength="64" /></label>{/if}{#if isLabel}<label>TV recent priority<select bind:value={torrentPriority.tv.recent}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>TV older priority<select bind:value={torrentPriority.tv.older}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.tv} />TV add paused</label>{/if}{#if isNzbget}<label>TV recent priority<select bind:value={nzbgetPriority.tv.recent}>{#each nzbgetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>TV older priority<select bind:value={nzbgetPriority.tv.older}>{#each nzbgetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.tv} />TV add paused</label>{/if}{#if implementation === 'sabnzbd'}<label>TV recent priority<select bind:value={usenetPriority.tv.recent}>{#each usenetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>TV older priority<select bind:value={usenetPriority.tv.older}>{#each usenetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label>{/if}<label>TV client eligibility tags<select multiple bind:value={tvTags} disabled={tagLoading||tagsStale||!tagLoaded}>{#each clientTags.tv as tag(tag.id)}<option value={tag.id}>{tag.label}</option>{/each}{#each tvTags.filter(id=>!clientTags.tv.some(tag=>tag.id===id)) as id(id)}<option value={id}>Unavailable tag {id}</option>{/each}</select></label>{:else}<label>TV categories<input bind:value={tvCategories} placeholder="5000" /></label><label>Anime categories<input bind:value={animeCategories} placeholder="5070" /></label><label class="check"><input type="checkbox" bind:checked={tvOptions.enable_rss} />TV RSS</label><label class="check"><input type="checkbox" bind:checked={tvOptions.enable_automatic_search} />TV automatic search</label><label class="check"><input type="checkbox" bind:checked={tvOptions.enable_interactive_search} />TV interactive search</label>
              <label>TV bound download client<select bind:value={tvOptions.download_client_id}><option value={null}>Any enabled TV client</option>{#if tvOptions.download_client_id&&!page?.items.some(p=>p.id===tvOptions.download_client_id&&eligibleDownloadClient(p,'tv',implementation))}<option value={tvOptions.download_client_id}>Saved client {tvOptions.download_client_id} (unavailable on this page)</option>{/if}{#each page?.items.filter(p=>eligibleDownloadClient(p,'tv',implementation))??[] as client(client.id)}<option value={client.id}>{client.name}</option>{/each}</select></label>{/if}{/if}
            <label class="check"><input type="checkbox" bind:checked={movies} />Movie scope</label>
            {#if movies}{#if isClient}{#if isTransmission}<label>Movie placement<select bind:value={placement.movies}><option value="category">Category subfolder and label</option><option value="directory">Download directory</option></select></label>{#if placement.movies==='category'}<label>Movie download category<input bind:value={movieCategory} required maxlength="64" /></label>{:else}<label>Movie download directory<input bind:value={movieDirectory} required maxlength="1024" placeholder="/downloads/movies" /></label>{/if}<label>Movie recent priority<select bind:value={torrentPriority.movies.recent}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>Movie older priority<select bind:value={torrentPriority.movies.older}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.movies} />Movie add paused</label>{:else}<label>Movie download category<input bind:value={movieCategory} required maxlength="64" /></label>{/if}{#if isLabel}<label>Movie recent priority<select bind:value={torrentPriority.movies.recent}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>Movie older priority<select bind:value={torrentPriority.movies.older}>{#each queuePriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.movies} />Movie add paused</label>{/if}{#if isNzbget}<label>Movie recent priority<select bind:value={nzbgetPriority.movies.recent}>{#each nzbgetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>Movie older priority<select bind:value={nzbgetPriority.movies.older}>{#each nzbgetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label class="check"><input type="checkbox" bind:checked={addPaused.movies} />Movie add paused</label>{/if}{#if implementation === 'sabnzbd'}<label>Movie recent priority<select bind:value={usenetPriority.movies.recent}>{#each usenetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label><label>Movie older priority<select bind:value={usenetPriority.movies.older}>{#each usenetPriorities as option(option.value)}<option value={option.value}>{option.label}</option>{/each}</select></label>{/if}<label>Movie client eligibility tags<select multiple bind:value={movieTags} disabled={tagLoading||tagsStale||!tagLoaded}>{#each clientTags.movies as tag(tag.id)}<option value={tag.id}>{tag.label}</option>{/each}{#each movieTags.filter(id=>!clientTags.movies.some(tag=>tag.id===id)) as id(id)}<option value={id}>Unavailable tag {id}</option>{/each}</select></label>{:else}<label>Movie categories<input bind:value={movieCategories} required placeholder="2000" /></label><label class="check"><input type="checkbox" bind:checked={movieOptions.enable_rss} />Movie RSS</label><label class="check"><input type="checkbox" bind:checked={movieOptions.enable_automatic_search} />Movie automatic search</label><label class="check"><input type="checkbox" bind:checked={movieOptions.enable_interactive_search} />Movie interactive search</label>
              <label>Movie bound download client<select bind:value={movieOptions.download_client_id}><option value={null}>Any enabled Movie client</option>{#if movieOptions.download_client_id&&!page?.items.some(p=>p.id===movieOptions.download_client_id&&eligibleDownloadClient(p,'movies',implementation))}<option value={movieOptions.download_client_id}>Saved client {movieOptions.download_client_id} (unavailable on this page)</option>{/if}{#each page?.items.filter(p=>eligibleDownloadClient(p,'movies',implementation))??[] as client(client.id)}<option value={client.id}>{client.name}</option>{/each}</select></label>{/if}{/if}
            {#if isClient}<button type="button" disabled={locked||tagLoading} onclick={loadClientTags}>Reload client tag choices</button>{#if tagLoading}<p role="status">Loading client tags…</p>{/if}{#if tagError}<p role="alert">{tagError}</p>{/if}{#if tagsStale&&tagLoaded}<p role="alert">Tag choices changed. Reload and review client tags.</p>{/if}<p>Default selection prefers clients whose eligibility tags match the series or movie. Untagged clients are used when no tagged client matches. These tags do not add labels to remote downloads.</p>{/if}
            <p>{isLabel ? 'Use distinct labels for TV and movies (lowercase letters, digits, dots, underscores and hyphens). A label is created in Deluge on the first grab if it does not exist yet; rTorrent stores it in d.custom1 as ruTorrent does. First moves new torrents to the top of the queue; recent priority applies to recently aired episodes and recently released movies.' : implementation === 'transmission' ? 'Give TV and movies distinct categories (a subfolder of the Transmission download directory, also applied as a label on Transmission 4.0 and later) or distinct, non-nested absolute download directories on the Transmission host. First moves new torrents to the top of the queue; recent priority applies to recently aired episodes and recently released movies.' : implementation === 'nzbget' ? 'Use distinct NZBGet categories for TV and movies; each must already exist in NZBGet. Jobs are added by URL with the category and priority above; recent priority applies to episodes and movies released recently, older priority otherwise. Jobs added paused must be resumed in NZBGet.' : implementation === 'sabnzbd' ? 'Use distinct SABnzbd categories for TV and movies; each must already exist in SABnzbd with job folders enabled and sorting disabled. Recent priority applies to episodes and movies released recently; older priority applies otherwise.' : implementation === 'qbittorrent'  ? 'Use distinct, non-overlapping categories for TV and movies. Existing imported categories, priorities and download options are preserved.' : 'Enter the numeric category IDs advertised by your indexer, separated by commas. Use provider-list pages to browse client choices. A saved client binding is preserved even when unavailable; default selection requires that client to be eligible for this scope; an explicit client choice overrides the preference. Other search options are preserved.'} Disabling a scope removes its saved settings.</p>
          </fieldset>
          <fieldset disabled={locked || conflict}><legend>Credentials</legend>
            <label>Credential action<select bind:value={credentialAction} onchange={clearSecrets}><option value="preserve">{selected ? 'Preserve stored credentials' : 'No credentials'}</option><option value="replace">Replace entire credential bundle</option><option value="clear">Clear entire credential bundle</option></select></label>
            {#if credentialAction !== 'preserve'}<p class="credential-warning">{credentialAction === 'clear' ? 'Clearing removes' : 'Replacing discards'} the entire saved credential bundle, including any private TV/movie parameters not shown here.</p>{/if}
            {#if credentialAction === 'replace'}{#if implementation === 'transmission'}<p>Transmission uses its RPC username and password (HTTP basic authentication).</p>{/if}{#if implementation === 'deluge'}<p>Deluge uses the Web UI password; there is no username.</p>{/if}{#if implementation === 'rtorrent'}<p>rTorrent itself has no authentication; use the HTTP basic username and password of the web server in front of it, or no credentials if it requires none.</p>{/if}{#if isNzbget}<p>NZBGet uses its ControlUsername and ControlPassword (HTTP basic authentication). Leave credentials unset only if NZBGet requires none.</p>{/if}{#if implementation === 'qbittorrent'}<label>Credential type<select bind:value={credentialKind} onchange={clearSecrets}><option value="username_password">Username and password</option><option value="api_key">API key</option></select></label>{/if}{#if (implementation === 'qbittorrent' && credentialKind === 'username_password') || implementation === 'transmission' || implementation === 'nzbget' || implementation === 'rtorrent'}<label>Username<input bind:value={username} autocomplete="off" required maxlength="4096" /></label><label>Password<input type="password" bind:value={password} autocomplete="new-password" required maxlength="4096" /></label>{:else}<label>API key<input type="password" bind:value={apiKey} autocomplete="new-password" required maxlength="4096" /></label>{/if}{/if}
          </fieldset>
          <button class="primary" disabled={locked || conflict || !dirty || (!selected && !template) || (isClient&&(!tagLoaded||tagLoading||tagsStale||!!tagError))}>Save provider</button>
        </form>
        {#if selected}<div class="actions"><button disabled={locked || dirty || conflict || !selected.test_supported} onclick={test}>Test saved provider</button><button disabled={locked || conflict} onclick={() => confirmingDelete = !confirmingDelete}>Delete provider</button></div>{#if dirty}<p>Save or reload changes before testing. Tests always use the saved configuration.</p>{/if}{/if}
        {#if confirmingDelete}<div class="delete-confirm"><p>Delete configuration for {selected?.name}? This removes both saved scopes and credentials. It does not delete media or remote downloads.</p><button disabled={busy} onclick={remove}>Confirm delete configuration</button><button disabled={busy} onclick={() => confirmingDelete = false}>Cancel delete</button></div>{/if}
        {#if testResult}<div role="status"><h3>Connection test result</h3><p>Tested revision {testResult.revision}: {testResult.result.domains.join(', ')}.</p>{#if 'sabnzbd_version' in testResult.result}<p>SABnzbd {testResult.result.sabnzbd_version}. Configured categories exist with job folders and no sorting.</p>{#each testResult.result.warnings as warning(warning)}<p role="alert">{warning === 'develop_version' ? 'This is a development build of SABnzbd; its version cannot be compared.' : 'SABnzbd removes completed downloads from history; completed jobs may disappear before import.'}</p>{/each}{:else if 'nzbget_version' in testResult.result}<p>NZBGet {testResult.result.nzbget_version}. Configured categories exist and job lists are readable.</p>{#each testResult.result.warnings as warning(warning)}<p role="alert">{warning === 'testing_version' ? 'This is a testing build of NZBGet; its version cannot be compared.' : 'NZBGet keeps history for under two days; completed jobs may disappear before import.'}</p>{/each}{:else if 'transmission_version' in testResult.result}<p>Transmission {testResult.result.transmission_version}; RPC {testResult.result.rpc_version}. {testResult.result.labels_supported ? 'Labels identify each domain’s category torrents.' : 'This version has no labels; category torrents are recognised by their download folder.'}</p>{#each testResult.result.warnings as warning(warning)}<p role="alert">{warning === 'labels_unsupported' ? 'Transmission is older than 4.0: category scopes are isolated by subfolder only.' : 'A scope moves new torrents to the top of the queue, but the Transmission download queue is disabled.'}</p>{/each}{:else if 'client_version' in testResult.result}<p>{implementation==='rtorrent'?'rTorrent':'Deluge'} {testResult.result.client_version}. {implementation==='rtorrent'?'Torrent lists are readable for each configured label.':'The Label plugin is enabled and torrent lists are readable for each configured label.'}</p>{:else if 'missing_categories' in testResult.result}<p>qBittorrent {testResult.result.application_version}; API {testResult.result.api_version}. Queueing {testResult.result.queueing_enabled ? 'enabled' : 'disabled'}.</p><p>{testResult.result.missing_categories.length ? `Missing categories: ${testResult.result.missing_categories.join(', ')}. These were not created by this test.` : 'Configured categories exist.'}</p>{:else}<p>Indexer capabilities and scoped feed probes passed.</p>{/if}<p>This is a connection observation, not proof that downloads or imports work.</p></div>{/if}
      {:else}<h2>Select a provider</h2><p>Review saved settings or add Torznab, Newznab, qBittorrent, SABnzbd, NZBGet, Transmission, Deluge or rTorrent.</p>{/if}
    </article>
  </div>
</section>
<style>
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem}.providers-workspace{display:grid;grid-template-columns:minmax(240px,320px) minmax(0,1fr);background:#fff;border:1px solid #cbd5de}aside{padding:1.25rem;border-right:1px solid #cbd5de}article{padding:1.5rem;min-width:0}ul{list-style:none;padding:0}li{border-bottom:1px solid #dce3e9}li button{width:100%;text-align:left;border:0;border-radius:0;padding:.8rem .4rem}li span{display:block;font-size:.85rem;color:#596b7b}fieldset{display:grid;gap:.8rem;border:1px solid #cbd5de;padding:1rem;margin:0;min-width:0}legend{font-weight:700}.check{display:flex;align-items:center;gap:.6rem}.check input{width:auto}fieldset p{margin:0;font-size:.9rem}.pagination{display:flex;gap:.5rem;margin-top:1rem}.pagination button{font-size:.8rem}.credential-warning,.delete-confirm{border-left:3px solid #a12637;padding:.8rem;background:#fff0f2}.delete-confirm{margin-top:1rem}.delete-confirm button{margin-right:.5rem}@media(max-width:800px){.providers-workspace{grid-template-columns:1fr}aside{border-right:0;border-bottom:1px solid #cbd5de}article{padding:1rem}.heading{flex-wrap:wrap}}
</style>
