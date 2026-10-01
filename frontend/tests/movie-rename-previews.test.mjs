import test from 'node:test';
import assert from 'node:assert/strict';
import {getMovieRenamePreview} from '../src/lib/api.ts';

test('movie rename preview GET preserves complete captured response and sorts a copy of selected IDs',async()=>{
 const original=globalThis.fetch,calls=[];
 const value={naming_revision:2,rename_enabled:true,standard_movie_format:'{Movie Title}',movie_ids:[1,3],files_considered:2,unchanged_count:0,unavailable_count:1,items:[{movie_id:1,movie_file_id:9,existing_path:'old.mkv',new_path:'New.mkv',status:'change',reasons:[]},{movie_id:3,movie_file_id:1,existing_path:null,new_path:null,status:'unavailable',reasons:['invalid_path']}]};
 globalThis.fetch=async(path,options)=>{calls.push([path,options.method,options.body]);return Response.json(value);};
 try{const selected=[3,1];assert.deepEqual(await getMovieRenamePreview(selected),{ok:true,data:value});assert.deepEqual(selected,[3,1]);assert.deepEqual(calls,[['/api/v1/movies/rename-preview?movie_ids=1%2C3',undefined,undefined]]);}finally{globalThis.fetch=original;}
});
test('movie rename preview rejects incomplete, duplicate and unsafe selections before fetch',async()=>{
 const original=globalThis.fetch;let calls=0;globalThis.fetch=async()=>{calls++;return Response.json({});};
 try {for(const ids of [[],[1,1],[0],[-1],[1.5],[NaN],[Infinity],[9007199254740992],Array.from({length:201},(_,i)=>i+1)])assert.equal((await getMovieRenamePreview(ids)).ok,false,JSON.stringify(ids));assert.equal(calls,0);
 await getMovieRenamePreview(Array.from({length:200},(_,i)=>i+1));await getMovieRenamePreview([9007199254740991]);assert.equal(calls,2);
 }finally{globalThis.fetch=original;}
});
test('movie rename preview preserves native failure status/code and rejects invalid transport data',async()=>{
 const original=globalThis.fetch;
 try {for(const [status,code] of [[404,'movie_not_found'],[422,'naming_configuration_invalid'],[500,'database_error'],[503,'rename_preview_unavailable']]){globalThis.fetch=async()=>Response.json({error:{code,message:'Preview unavailable'}},{status});assert.deepEqual(await getMovieRenamePreview([1]),{ok:false,status,code,error:'Preview unavailable'});}
 for(const response of [()=>new Response('not json'),()=>Response.json({naming_revision:9007199254740992})]){globalThis.fetch=async()=>response();assert.equal((await getMovieRenamePreview([1])).ok,false);}
 globalThis.fetch=async()=>{throw new TypeError('owned disconnect');};assert.equal((await getMovieRenamePreview([1])).ok,false);
 }finally{globalThis.fetch=original;}
});
