BEGIN TRANSACTION;
CREATE TABLE account_deletions(id INTEGER PRIMARY KEY AUTOINCREMENT,account_id INTEGER NOT NULL,settlement_id INTEGER NOT NULL UNIQUE,account_name TEXT NOT NULL,account_created_at TEXT NOT NULL,actor_id INTEGER NOT NULL,session_id TEXT,status TEXT NOT NULL DEFAULT 'queued',version INTEGER NOT NULL DEFAULT 1,error TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE TABLE accounts(id INTEGER PRIMARY KEY,name TEXT NOT NULL,root_id INTEGER NOT NULL,platform TEXT NOT NULL,status TEXT NOT NULL,deleted INTEGER NOT NULL,created_at TEXT NOT NULL);
INSERT INTO "accounts" VALUES(100,'Fixture root 100',100,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(101,'Fixture child 101',100,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(102,'Fixture child 102',100,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(200,'Fixture root 200',200,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(300,'Fixture root 300',300,'grok','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(400,'Fixture root 400',400,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
INSERT INTO "accounts" VALUES(500,'Fixture root 500',500,'openai','active',0,'2024-12-01T00:00:00.000000+00:00');
CREATE TABLE audit(id INTEGER PRIMARY KEY AUTOINCREMENT,actor_id INTEGER NOT NULL,action TEXT NOT NULL,entity_id INTEGER,payload TEXT NOT NULL,created_at TEXT NOT NULL);
INSERT INTO "audit" VALUES(1,1,'create_cycle',1,'{"cost_fen":1000,"cost_kind":"exact","creditor_id":2,"renewed_from_cycle_id":null,"start":"2025-01-01T00:00:00.000000+00:00","start_confirmed":1,"start_confirmed_at":null,"start_confirmed_by":null,"start_lower":"2025-01-01T00:00:00.000000+00:00","start_mode":"exact","start_source_usage_id":null,"start_upper":null}','2025-04-01T00:00:00.000000+00:00');
INSERT INTO "audit" VALUES(2,1,'settle',1,'{"cost_fen":1000,"settlement_id":1}','2025-04-01T00:00:00.000000+00:00');
CREATE TABLE batches(id INTEGER PRIMARY KEY AUTOINCREMENT,payer_id INTEGER NOT NULL,creditor_id INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'active',created_at TEXT NOT NULL);
CREATE TABLE bill_imports(id INTEGER PRIMARY KEY AUTOINCREMENT,cycle_id INTEGER NOT NULL,file_name TEXT NOT NULL,file_hash TEXT NOT NULL,rows TEXT NOT NULL,created_by INTEGER NOT NULL,created_at TEXT NOT NULL,UNIQUE(cycle_id,file_hash));
CREATE TABLE bill_previews(id TEXT PRIMARY KEY,actor_id INTEGER NOT NULL,cycle_id INTEGER NOT NULL,payload TEXT NOT NULL,expires_at TEXT NOT NULL);
CREATE TABLE "cycles"(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    cost_fen INTEGER NOT NULL,
    creditor_id INTEGER NOT NULL,
    start TEXT NOT NULL,
    end TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    version INTEGER NOT NULL DEFAULT 1,
    settlement_id INTEGER,
    start_mode TEXT NOT NULL DEFAULT 'exact',
    start_lower TEXT,
    start_upper TEXT,
    start_confirmed INTEGER NOT NULL DEFAULT 1,
    start_confirmed_by INTEGER,
    start_confirmed_at TEXT,
    start_source_usage_id INTEGER,
    renewed_from_cycle_id INTEGER
);
INSERT INTO "cycles" VALUES(1,100,'Fixture public 100',1000,2,'2025-01-01T00:00:00.000000+00:00','2025-02-01T00:00:00.000000+00:00','settled',2,1,'exact','2025-01-01T00:00:00.000000+00:00',NULL,1,NULL,NULL,NULL,NULL);
CREATE TABLE expenses(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,cost_fen INTEGER NOT NULL,incurred_on TEXT NOT NULL,creditor_id INTEGER NOT NULL,allocation_mode TEXT NOT NULL,members TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'open',version INTEGER NOT NULL DEFAULT 1,settlement_id INTEGER,created_at TEXT NOT NULL);
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
INSERT INTO "meta" VALUES('coverage_from','"2025-01-01T00:00:00.000000+00:00"');
INSERT INTO "meta" VALUES('source_ids','[1,2,3,4,5,6,7,8,9]');
INSERT INTO "meta" VALUES('archived_deletions','[]');
INSERT INTO "meta" VALUES('cleanup_counts','{}');
INSERT INTO "meta" VALUES('coverage_gaps','[]');
INSERT INTO "meta" VALUES('last_sync','"2025-04-01T00:00:00.000000+00:00"');
INSERT INTO "meta" VALUES('sync_error','null');
INSERT INTO "meta" VALUES('source_count','9');
CREATE TABLE netting_payments(id INTEGER PRIMARY KEY AUTOINCREMENT,user_a INTEGER NOT NULL,user_b INTEGER NOT NULL,payer_id INTEGER,creditor_id INTEGER,amount_fen INTEGER NOT NULL CHECK(amount_fen>=0),offset_fen INTEGER NOT NULL CHECK(offset_fen>=0),a_to_b_fen INTEGER NOT NULL,b_to_a_fen INTEGER NOT NULL,actor_id INTEGER NOT NULL,created_at TEXT NOT NULL,digest TEXT NOT NULL,snapshot TEXT NOT NULL);
CREATE TABLE notifications(id INTEGER PRIMARY KEY AUTOINCREMENT,user_id INTEGER NOT NULL,title TEXT NOT NULL,order_id INTEGER,read INTEGER NOT NULL DEFAULT 0,created_at TEXT NOT NULL);
INSERT INTO "notifications" VALUES(1,3,'新结算订单',1,0,'2025-04-01T00:00:00.000000+00:00');
INSERT INTO "notifications" VALUES(2,2,'新增应收订单',1,0,'2025-04-01T00:00:00.000000+00:00');
INSERT INTO "notifications" VALUES(3,4,'新结算订单',2,0,'2025-04-01T00:00:00.000000+00:00');
INSERT INTO "notifications" VALUES(4,2,'新增应收订单',2,0,'2025-04-01T00:00:00.000000+00:00');
CREATE TABLE "orders"(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    settlement_id INTEGER NOT NULL,
    cycle_id INTEGER NOT NULL,
    payer_id INTEGER NOT NULL,
    creditor_id INTEGER NOT NULL,
    payer_name TEXT NOT NULL,
    creditor_name TEXT NOT NULL,
    account_name TEXT NOT NULL,
    amount_fen INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    batch_id INTEGER,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    submitted_at TEXT,
    paid_at TEXT
);
INSERT INTO "orders" VALUES(1,1,1,3,2,'Fixture Payer','Fixture Creditor','Fixture public 100',500,'pending',NULL,1,'2025-04-01T00:00:00.000000+00:00',NULL,NULL);
INSERT INTO "orders" VALUES(2,1,1,4,2,'Fixture Other','Fixture Creditor','Fixture public 100',400,'pending',NULL,1,'2025-04-01T00:00:00.000000+00:00',NULL,NULL);
CREATE TABLE previews(id TEXT PRIMARY KEY,actor_id INTEGER NOT NULL,payload TEXT NOT NULL,expires_at TEXT NOT NULL);
INSERT INTO "previews" VALUES('b99a789aaddf396b0e395f62a5387f27',1,'[{"allocations":[{"amount_fen":100,"consumption":"1","self_covered":true,"share":"0.1","user_id":2,"user_name":"Fixture Creditor"},{"amount_fen":500,"consumption":"5","self_covered":false,"share":"0.5","user_id":3,"user_name":"Fixture Payer"},{"amount_fen":400,"consumption":"4","self_covered":false,"share":"0.4","user_id":4,"user_name":"Fixture Other"}],"can_settle":true,"creditor_name":"Fixture Creditor","cycle":{"account_id":100,"bill_allocation_mode":"proportional","bill_difference_confirmed":0,"bill_digest":null,"bill_end":null,"bill_import_id":null,"billable_cost_fen":null,"cost_fen":1000,"cost_kind":"exact","cost_source":"manual","creditor_id":2,"end":null,"id":1,"name":"Fixture public 100","renewed_from_cycle_id":null,"settlement_id":null,"start":"2025-01-01T00:00:00.000000+00:00","start_anchor":null,"start_confirmed":1,"start_confirmed_at":null,"start_confirmed_by":null,"start_locked":0,"start_lower":"2025-01-01T00:00:00.000000+00:00","start_mode":"exact","start_source_usage_id":null,"start_upper":null,"status":"open","version":1},"data_quality":"complete","digest":"c59ddd75fb75ff9660d510303959257414ad8a2e3a3bab3a781dba5ced2f38cf","end":"2025-02-01T00:00:00.000000+00:00","record_count":4,"start_confirmed":true,"start_confirmed_at":null,"start_confirmed_by":null,"start_lower":"2025-01-01T00:00:00.000000+00:00","start_mode":"exact","start_source_usage_id":null,"start_upper":null,"start_warning":null,"total_consumption":"10"}]','2025-04-01T00:10:00.000000+00:00');
CREATE TABLE profiles(account_id INTEGER PRIMARY KEY,name TEXT NOT NULL,kind TEXT NOT NULL CHECK(kind IN ('paid','free')),version INTEGER NOT NULL DEFAULT 1);
INSERT INTO "profiles" VALUES(100,'Fixture public 100','paid',1);
CREATE TABLE requests(actor_id INTEGER NOT NULL,key TEXT NOT NULL,signature TEXT NOT NULL,response TEXT NOT NULL,PRIMARY KEY(actor_id,key));
INSERT INTO "requests" VALUES(1,'migration-old-cycle','c4aa758a356e1cbb9947b95fac372d7bf24eff76d28dde28a9b58477f0ae0880','{"id":1}');
INSERT INTO "requests" VALUES(1,'migration-old-settle','8bcd6db023c6051c37501eab7669d5468ed6f207da6c0e6891d3f1e65145b830','{"settlements":[{"cycle_id":1,"settlement_id":1}]}');
CREATE TABLE sessions(id TEXT PRIMARY KEY,user_id INTEGER NOT NULL,token TEXT NOT NULL,csrf TEXT NOT NULL,expires_at REAL NOT NULL);
INSERT INTO "sessions" VALUES('5b7f8daf2a2049a4cdf8d829fb7dd86d4a7c6bca6b72209e8852caa1a9dd1c86',1,'gAAAAABn6yyAuDHvxgDqMxsJcjVGe8CofhKvRj2iD9Sud1YtRus8XoxLpvcjSGmCBiJeYBi_8w9-JF_goSZIm1yHIj0mEhDWjA==','rcDUdwRPFw9ia23AjkJYWEx1NBolzie-5vfvV3qBdbI',1743494400.0);
INSERT INTO "sessions" VALUES('f1c0768a5ecc81be5f203c991a0efe6f85fee9847eed8412c044f19706687a49',3,'gAAAAABn6yyAgv8PfH2ey-LR0EpzNtnRowS3zsHSj2nLrJieM47nDqRIT3vth7YG4d-gbqskNztfLsE1pncIxvFo4G0oNECLgQ==','b-rkBOFwehYUBQG36eBDzVAAkWL-LH1abQHAdSa9Ibw',1743494400.0);
CREATE TABLE settlement_usage(settlement_id INTEGER NOT NULL,usage_id INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(settlement_id,usage_id));
INSERT INTO "settlement_usage" VALUES(1,1,'{"account_id":100,"amount":"1","created_at":"2025-01-01T00:00:00.000000+00:00","id":1,"root_id":100,"user_id":2}');
INSERT INTO "settlement_usage" VALUES(1,2,'{"account_id":100,"amount":"3","created_at":"2025-01-10T00:00:00.000000+00:00","id":2,"root_id":100,"user_id":3}');
INSERT INTO "settlement_usage" VALUES(1,3,'{"account_id":101,"amount":"2","created_at":"2025-01-15T00:00:00.000000+00:00","id":3,"root_id":100,"user_id":3}');
INSERT INTO "settlement_usage" VALUES(1,4,'{"account_id":102,"amount":"4","created_at":"2025-01-20T00:00:00.000000+00:00","id":4,"root_id":100,"user_id":4}');
CREATE TABLE "settlements"(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cycle_id INTEGER NOT NULL,
    snapshot TEXT NOT NULL,
    digest TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    drift INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
INSERT INTO "settlements" VALUES(1,1,'{"allocations":[{"amount_fen":100,"consumption":"1","self_covered":true,"share":"0.1","user_id":2,"user_name":"Fixture Creditor"},{"amount_fen":500,"consumption":"5","self_covered":false,"share":"0.5","user_id":3,"user_name":"Fixture Payer"},{"amount_fen":400,"consumption":"4","self_covered":false,"share":"0.4","user_id":4,"user_name":"Fixture Other"}],"can_settle":true,"cycle":{"account_id":100,"bill_allocation_mode":"proportional","bill_difference_confirmed":0,"bill_digest":null,"bill_end":null,"bill_import_id":null,"billable_cost_fen":null,"cost_fen":1000,"cost_kind":"exact","cost_source":"manual","creditor_id":2,"end":null,"id":1,"name":"Fixture public 100","renewed_from_cycle_id":null,"settlement_id":null,"start":"2025-01-01T00:00:00.000000+00:00","start_anchor":null,"start_confirmed":1,"start_confirmed_at":null,"start_confirmed_by":null,"start_locked":0,"start_lower":"2025-01-01T00:00:00.000000+00:00","start_mode":"exact","start_source_usage_id":null,"start_upper":null,"status":"open","version":1},"data_quality":"complete","digest":"c59ddd75fb75ff9660d510303959257414ad8a2e3a3bab3a781dba5ced2f38cf","end":"2025-02-01T00:00:00.000000+00:00","record_count":4,"start_confirmed":true,"start_confirmed_at":null,"start_confirmed_by":null,"start_lower":"2025-01-01T00:00:00.000000+00:00","start_mode":"exact","start_source_usage_id":null,"start_upper":null,"start_warning":null,"total_consumption":"10"}','c59ddd75fb75ff9660d510303959257414ad8a2e3a3bab3a781dba5ced2f38cf','active',0,'2025-04-01T00:00:00.000000+00:00');
CREATE TABLE usage(id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL,root_id INTEGER NOT NULL,user_id INTEGER NOT NULL,amount TEXT NOT NULL,created_at TEXT NOT NULL);
INSERT INTO "usage" VALUES(1,100,100,2,'1','2025-01-01T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(2,100,100,3,'3','2025-01-10T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(3,101,100,3,'2','2025-01-15T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(4,102,100,4,'4','2025-01-20T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(5,100,100,3,'5','2025-02-01T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(6,200,200,3,'2','2025-01-05T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(7,300,300,4,'9','2025-01-05T00:00:00.000000+00:00');
INSERT INTO "usage" VALUES(8,400,400,3,'1.00000001','2025-01-10T06:30:12.000000+00:00');
INSERT INTO "usage" VALUES(9,500,500,4,'2.00000002','2025-01-10T06:30:30.000000+00:00');
CREATE TABLE usage_details(usage_id INTEGER PRIMARY KEY,payload TEXT NOT NULL);
INSERT INTO "usage_details" VALUES(1,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-1","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
INSERT INTO "usage_details" VALUES(2,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-2","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
INSERT INTO "usage_details" VALUES(3,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-3","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
INSERT INTO "usage_details" VALUES(4,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-4","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
INSERT INTO "usage_details" VALUES(5,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-5","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
INSERT INTO "usage_details" VALUES(6,'{"cache_creation_tokens":0,"cache_read_tokens":0,"input_tokens":10,"model":"fixture-model","output_tokens":20,"request_id":"fixture-6","requested_model":null,"upstream_model":null,"upstream_request_id":null,"upstream_response_model":null}');
CREATE TABLE users(id INTEGER PRIMARY KEY,name TEXT NOT NULL,role TEXT NOT NULL,status TEXT NOT NULL,deleted INTEGER NOT NULL DEFAULT 0);
INSERT INTO "users" VALUES(1,'Fixture Admin','admin','active',0);
INSERT INTO "users" VALUES(2,'Fixture Creditor','user','active',0);
INSERT INTO "users" VALUES(3,'Fixture Payer','user','active',0);
INSERT INTO "users" VALUES(4,'Fixture Other','user','active',0);
INSERT INTO "users" VALUES(5,'Fixture Stranger','user','active',0);
CREATE INDEX usage_root_time ON usage(root_id,created_at);
CREATE INDEX deletion_account_status ON account_deletions(account_id,status);
CREATE INDEX netting_participants ON netting_payments(user_a,user_b,id);
CREATE TRIGGER netting_immutable_update BEFORE UPDATE ON netting_payments BEGIN SELECT RAISE(ABORT,'netting payments are immutable'); END;
CREATE TRIGGER netting_immutable_delete BEFORE DELETE ON netting_payments BEGIN SELECT RAISE(ABORT,'netting payments are immutable'); END;
DELETE FROM "sqlite_sequence";
INSERT INTO "sqlite_sequence" VALUES('audit',2);
INSERT INTO "sqlite_sequence" VALUES('notifications',4);
INSERT INTO "sqlite_sequence" VALUES('cycles',901);
INSERT INTO "sqlite_sequence" VALUES('settlements',500);
INSERT INTO "sqlite_sequence" VALUES('orders',700);
COMMIT;
